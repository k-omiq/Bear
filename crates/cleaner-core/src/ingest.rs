//! Ingest: what a file is, without decoding it.
//!
//! A `SourceRef` comes from a header-only read, and the policy around it
//! shapes the module. The rule is
//!
//! > Never clean a partially-decoded page.
//!
//! so a file whose header does not parse, or whose pixels do not fully decode,
//! is **skipped and listed** rather than repaired. The listing is the product:
//! a job that silently drops files is worse than one that cleans nothing.
//!
//! PNG, TIFF and JPEG are imported byte for byte. WebP, GIF and BMP are
//! decoded and written as PNG into the chapter's own page directory. The
//! user's originals are never modified, and a copied source retains its hash.
//!
//! Every accepted page lives in the job's own directory, with
//! [`SourceRef::converted_from`] recording its origin for both copies and
//! conversions. This lets the chapter survive deletion of the scan folder.
//!
//! The user's folder is still the *origin*, and the two questions that turn on
//! it - where an export's `<input>_cleaned/` sibling goes, and which files
//! "output never overwrites input" has to refuse - are answered from
//! `converted_from` rather than from the page's own path.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::image::foreign::{self, SourceFormat};
use crate::image::{BitDepth, ColorMode, Format, Raster};

/// What a source file is, from its header.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRef {
    pub orientation: crate::image::orientation::Orientation,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub mode: ColorMode,
    pub bit_depth: BitDepth,
    pub icc_bytes: Option<Vec<u8>>,
    pub sha256: String,
    /// The original file before copying or converting it into the chapter.
    /// `None` when ingest reads a native source in place.
    ///
    /// Kept because the original is still the user's input, and two rules turn
    /// on that: "output never overwrites input" has to refuse the JPEG as well
    /// as the PNG made from it, and the export's default `<input>_cleaned/`
    /// must be a sibling of the *scan folder* rather than of a directory
    /// inside the library.
    pub converted_from: Option<PathBuf>,
    pub conversion: Option<ConversionProvenance>,
}

/// Chapter-owned original and an explicit account of the working conversion.
/// Paths become manifest-relative in Project::new; older manifests default to None.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConversionProvenance {
    pub archived_original: PathBuf,
    pub original_sha256: String,
    pub working_sha256: String,
    pub detected_format: String,
    pub chosen_frame: u32,
    pub frame_count: u32,
    pub conversion_version: String,
    pub retained: Vec<String>,
    pub lost: Vec<String>,
}

/// Why a file did not make it in. Each carries the i18n key the seam reports
/// it under - no English crosses the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Not an image this application reads at all.
    NotAnImage,
    /// The header is there and does not parse.
    HeaderUnreadable(String),
    /// The header parses and the pixels do not - the truncated-scan case §2
    /// singles out.
    PartialDecode(String),
    /// A colour mode the fidelity contract has no policy for. Refused rather
    /// than promoted, because promotion is the failure §2 is written against.
    UnsupportedMode(String),
    /// Byte-identical to a file already accepted.
    Duplicate { of: PathBuf },
    /// Dotfiles, `__MACOSX/`, `Thumbs.db`, `desktop.ini`.
    Junk,
    /// The file decoded and the PNG made from it could not be written.
    ///
    /// Distinct from [`SkipReason::PartialDecode`] because it says nothing
    /// about the user's file: what failed is this application's own write into
    /// the job's sidecar, and a user told "it does not decode completely"
    /// about a perfectly good JPEG would go looking in the wrong place.
    ConversionFailed(String),
    /// The file was fine in every way and the library's own copy of it could
    /// not be written - a full disk, a read-only library directory.
    ///
    /// Separate from [`SkipReason::ConversionFailed`] for the reason that one
    /// is separate from `PartialDecode`: nothing was converted, so a user told
    /// "it could not be saved as a PNG" about their own PNG would go looking
    /// at a file that is perfectly good.
    ImportFailed(String),
}

impl SkipReason {
    pub fn reason_key(&self) -> &'static str {
        match self {
            SkipReason::NotAnImage => "input.skipReason.notAnImage",
            SkipReason::HeaderUnreadable(_) => "input.skipReason.headerUnreadable",
            SkipReason::PartialDecode(_) => "input.skipReason.partialDecode",
            SkipReason::UnsupportedMode(_) => "input.skipReason.unsupportedMode",
            SkipReason::Duplicate { .. } => "input.skipReason.duplicate",
            SkipReason::Junk => "input.skipReason.junk",
            SkipReason::ConversionFailed(_) => "input.skipReason.conversionFailed",
            SkipReason::ImportFailed(_) => "input.skipReason.importFailed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// Something worth telling the user that did not stop a file being accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// `001.jpg` beside `001.png`. §2: warn rather than guess.
    DuplicateBasename { stem: String, paths: Vec<PathBuf> },
}

/// One file that arrived in a format this application reads and cannot write,
/// and the PNG it became.
///
/// Separate from [`SourceRef::converted_from`], which answers a different
/// question: that field is how the *manifest* keeps hold of the original, and
/// this is what the chapter tells the user happened. Only this one knows what
/// the format was, because it was decided by the magic bytes and not by the
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converted {
    pub original: PathBuf,
    pub png: PathBuf,
    /// `JPEG`, `WebP`, `GIF`, `BMP` - [`SourceFormat::label`].
    pub from: &'static str,
}

#[derive(Debug, Default)]
pub struct IngestReport {
    pub sources: Vec<SourceRef>,
    pub skipped: Vec<Skipped>,
    pub warnings: Vec<Warning>,
    /// Every file converted on the way in, in the order they were converted.
    pub converted: Vec<Converted>,
}

impl IngestReport {
    /// §2's "empty result" case: a job that detects nothing produces output
    /// identical to input, which is indistinguishable from total failure.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn junk_skipped(&self) -> usize {
        self.skipped.iter().filter(|s| s.reason == SkipReason::Junk).count()
    }
}

/// The names §2 lists, plus anything beginning with a dot.
pub fn is_junk(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    if name.starts_with('.') {
        return true;
    }
    if matches!(name, "Thumbs.db" | "desktop.ini") {
        return true;
    }
    path.components().any(|c| c.as_os_str() == "__MACOSX")
}

/// Natural sort: digit runs compare as numbers, so `page2` precedes `page10`.
/// Lifted from the typesetter's `naturalSort`; the point is that a chapter's
/// page order is the file order and a lexical sort gets it wrong on the first
/// chapter with ten pages.
/// Case and zero-padding are **tiebreaks**, not part of the primary
/// comparison. Deciding them per character would sort `Page3` before `page1`,
/// because the case difference at position 0 fires before position 1 is looked
/// at - and `001` before `1a` for the same reason one step later. They are
/// recorded on first sight and applied only if everything else is equal, which
/// keeps the order total without letting them dominate it.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    let mut tiebreak = Ordering::Equal;

    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return tiebreak,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let mut anum = String::new();
                    while ai.peek().is_some_and(|c| c.is_ascii_digit()) {
                        anum.push(ai.next().expect("peeked"));
                    }
                    let mut bnum = String::new();
                    while bi.peek().is_some_and(|c| c.is_ascii_digit()) {
                        bnum.push(bi.next().expect("peeked"));
                    }
                    // Runs longer than a u128 compare by length then digits,
                    // which is the same answer for any real page number.
                    let numeric = match (anum.parse::<u128>(), bnum.parse::<u128>()) {
                        (Ok(an), Ok(bn)) => an.cmp(&bn),
                        _ => anum.trim_start_matches('0').len().cmp(&bnum.trim_start_matches('0').len()),
                    };
                    match numeric {
                        Ordering::Equal => {
                            if tiebreak == Ordering::Equal {
                                tiebreak = anum.cmp(&bnum);
                            }
                        }
                        other => return other,
                    }
                } else {
                    let (ac, bc) = (ai.next().expect("peeked"), bi.next().expect("peeked"));
                    match ac.to_ascii_lowercase().cmp(&bc.to_ascii_lowercase()) {
                        Ordering::Equal => {
                            if tiebreak == Ordering::Equal {
                                tiebreak = ac.cmp(&bc);
                            }
                        }
                        other => return other,
                    }
                }
            }
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read a file's header, without decoding its pixels.
///
/// PNG stops after `IHDR`…`IDAT`'s header, TIFF after the first IFD. Both are
/// cheap enough that a 200-page folder is listed in a blink, which is what
/// makes a header-only `SourceRef` worth having.
pub fn source_ref(path: &Path, bytes: &[u8]) -> Result<SourceRef, SkipReason> {
    Format::sniff(bytes).ok_or(SkipReason::NotAnImage)?;
    let header = crate::image::header(bytes)
    .map_err(|e| SkipReason::HeaderUnreadable(e.to_string()))?;

    Ok(SourceRef {
        orientation: crate::image::orientation::from_bytes(bytes),
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        mode: header.mode,
        bit_depth: header.depth,
        icc_bytes: header.icc,
        sha256: sha256_hex(bytes),
        converted_from: None,
        conversion: None,
    })
}

/// Decode fully, and treat any failure as a skip rather than as an error to
/// recover from. This is the "never clean a partially-decoded page" rule: the
/// decoders return an error on truncation rather than a short buffer, and the
/// caller must not be given the chance to use what arrived.
pub fn decode_whole(bytes: &[u8]) -> Result<Raster, SkipReason> {
    crate::image::decode(bytes).map_err(|e| match e {
        crate::image::ImageError::UnknownFormat => SkipReason::NotAnImage,
        crate::image::ImageError::Unrepresentable { .. } => SkipReason::UnsupportedMode(e.to_string()),
        other => SkipReason::PartialDecode(other.to_string()),
    })
}

/// §2's policy, held across a run of files.
///
/// Split out of [`ingest`] so that the same rules can be applied to a folder
/// **one file's bytes at a time** ([`ingest_paths`]) as to a list of buffers
/// already in hand ([`ingest`]). The state it carries between files is the
/// whole reason the two cannot simply be different loops: the duplicate check
/// is by sha256 against every file accepted so far, and the basename warning is
/// against every stem seen so far.
#[derive(Default)]
struct Sieve {
    report: IngestReport,
    by_hash: Vec<(String, PathBuf)>,
    by_stem: Vec<(String, PathBuf)>,
    /// The job's own page directory: where an accepted file is copied to, and
    /// where a PNG made from WebP, GIF or BMP is written. `None` reads
    /// the user's folder in place and refuses formats that need conversion.
    ///
    /// An option rather than a required argument because the two callers want
    /// genuinely different things: a chapter being created has a sidecar
    /// directory to put the pages in and wants them there, and a bare
    /// [`ingest`] over buffers in memory has nowhere to write and must not
    /// invent one.
    import_into: Option<PathBuf>,
}

impl Sieve {
    fn skip(&mut self, path: &Path, reason: SkipReason) {
        self.report.skipped.push(Skipped { path: path.to_path_buf(), reason });
    }

    /// §2 for one file whose bytes are in hand.
    ///
    /// Junk is the *caller's* check, not this function's: a caller reading from
    /// disk can answer it from the name alone and must not read the file to
    /// find out.
    ///
    /// A non-native source is checked against the supported conversion codecs
    /// in [`crate::image::foreign`] and becomes a PNG if there
    /// is somewhere to put it. Everything after that point is the same policy
    /// over the same bytes, because what is offered from there on **is** a PNG:
    /// the duplicate check, the stem warning and the manifest row all see the
    /// converted file, which is the file the rest of the pipeline will open.
    fn offer(&mut self, path: &Path, bytes: &[u8]) {
        let Some(format) = Format::sniff(bytes) else {
            return match (SourceFormat::sniff(bytes), self.import_into.clone()) {
                (Some(foreign), Some(dir)) => self.convert(path, bytes, foreign, &dir),
                _ => self.skip(path, SkipReason::NotAnImage),
            };
        };
        let mut source = match source_ref(path, bytes) {
            Ok(source) => source,
            Err(reason) => return self.skip(path, reason),
        };

        if self.is_duplicate(path, &source.sha256) {
            return;
        }

        // The header parsed; make sure the pixels do too, before anything
        // downstream is allowed to assume a whole page.
        if let Err(reason) = decode_whole(bytes) {
            return self.skip(path, reason);
        }

        // Last, so that nothing this sieve refuses ever leaves a file behind:
        // a duplicate and a truncated scan are both decided above, on bytes
        // alone, and cost the library nothing.
        if let Some(dir) = self.import_into.clone() {
            let extension = match format {
                Format::Png => "png",
                Format::Tiff => "tiff",
                Format::Jpeg => {
                    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpeg")) {
                        "jpeg"
                    } else {
                        "jpg"
                    }
                },
            };
            let written = free_path(&dir, path, extension);
            if let Err(error) = write_import(&dir, &written, bytes) {
                return self.skip(path, SkipReason::ImportFailed(error));
            }
            // A verbatim copy, so the hash the header read is still the file's
            // hash and nothing has to be read back to know it.
            source.converted_from = Some(path.to_path_buf());
            source.path = written;
        }

        // The *original* path, so the stem warning and the sources list agree
        // with what the user's folder is called.
        self.accept(path, source);
    }

    /// Decode a foreign file, write the PNG, and offer *that* to the rest of
    /// §2's policy.
    ///
    /// The order is: decode, encode, check for a duplicate, **then** write.
    /// Checking before writing is what keeps a folder holding the same JPEG
    /// twice from leaving a stray PNG behind - the second copy is refused as a
    /// duplicate having produced no file at all, rather than being written and
    /// then deleted, which is a cleanup that has to succeed to be correct.
    ///
    /// Peak memory is one page's file, its raster and its PNG. That is the
    /// same order as the decode [`offer`] already performs on every accepted
    /// file, so a converting ingest is not a new memory shape - the raster and
    /// the encoded bytes both die before the next file is read.
    ///
    /// [`offer`]: Sieve::offer
    fn convert(&mut self, path: &Path, bytes: &[u8], format: SourceFormat, dir: &Path) {
        let frames = match crate::image::import_info::frame_count(bytes) {
            Ok(frames) => frames,
            Err(error) => return self.skip(path, SkipReason::PartialDecode(error.to_string())),
        };
        let raster = match foreign::decode(bytes) {
            Ok(raster) => raster,
            // The pixels did not come out, which is the same fact
            // `decode_whole` reports about a truncated PNG and reads the same
            // way to a user looking at a half-downloaded scan.
            Err(error) => return self.skip(path, SkipReason::PartialDecode(error.to_string())),
        };
        // Use a lossless encoder compatible with the decoded raster.
        let target = crate::image::lossless_format_for(&raster);
        let encoded = match crate::image::encode(&raster, target) {
            Ok(encoded) => encoded,
            Err(error) => return self.skip(path, SkipReason::ConversionFailed(error.to_string())),
        };
        let indexed = raster.mode == ColorMode::Indexed;
        let profiled = raster.icc.is_some();
        let exif = raster.color.safe_ancillary.iter().any(|chunk| chunk.name == *b"eXIf");
        drop(raster);

        let sha256 = sha256_hex(&encoded);
        if self.is_duplicate(path, &sha256) {
            return;
        }

        let extension = match target {
            Format::Png => "png",
            Format::Tiff => "tiff",
            Format::Jpeg => unreachable!("conversion always selects a lossless format"),
        };
        let written = free_path(dir, path, extension);
        // Validate before publishing either asset; pair cleanup handles a second
        // write failure without ever adding half an import to the report.
        let mut source = match source_ref(&written, &encoded) {
            Ok(source) => source,
            Err(reason) => return self.skip(path, reason),
        };
        let originals = dir.parent().unwrap_or(dir).join("originals");
        let original_extension = match format { SourceFormat::Jpeg => "jpg", SourceFormat::Webp => "webp", SourceFormat::Gif => "gif", SourceFormat::Bmp => "bmp" };
        let archived = free_path(&originals, &written, original_extension);
        if let Err(error) = write_import(&originals, &archived, bytes) {
            return self.skip(path, SkipReason::ConversionFailed(error));
        }
        if let Err(error) = write_import(dir, &written, &encoded) {
            let _ = std::fs::remove_file(&archived);
            return self.skip(path, SkipReason::ConversionFailed(error));
        }
        let mut retained = vec!["visible first-frame samples".into(), "complete original archive".into()];
        if indexed { retained.push("palette, indices and transparency".into()); }
        if profiled { retained.push("ICC profile".into()); }
        if exif { retained.push("EXIF metadata and native orientation".into()); }
        let mut lost = vec!["source container and nonrepresentable ancillary fields (retained in archive)".into()];
        if frames > 1 { lost.push("animation timing and subsequent frames (retained in archive)".into()); }
        source.conversion = Some(ConversionProvenance {
            archived_original: archived, original_sha256: sha256_hex(bytes), working_sha256: sha256,
            detected_format: format.label().into(), chosen_frame: 0, frame_count: frames,
            conversion_version: "native-first-frame-v1".into(), retained, lost,
        });
        source.converted_from = Some(path.to_path_buf());
        self.report.converted.push(Converted {
            original: path.to_path_buf(),
            png: written,
            from: format.label(),
        });
        // The *original* path, so the stem warning and the sources list agree
        // with what the user's folder is called.
        self.accept(path, source);
    }

    /// Whether this content has already been accepted, recording the skip if so.
    fn is_duplicate(&mut self, path: &Path, sha256: &str) -> bool {
        let Some((_, first)) = self.by_hash.iter().find(|(hash, _)| hash == sha256) else {
            return false;
        };
        let of = first.clone();
        self.skip(path, SkipReason::Duplicate { of });
        true
    }

    /// The last half of §2, over a source that has passed every check.
    /// `path` is the file the *user* has, which for a converted page is the
    /// original rather than `source.path`.
    fn accept(&mut self, path: &Path, source: SourceRef) {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if let Some((_, other)) = self.by_stem.iter().find(|(seen, _)| seen == stem) {
                let other = other.clone();
                match self.report.warnings.iter_mut().find(
                    |w| matches!(w, Warning::DuplicateBasename { stem: s, .. } if s == stem),
                ) {
                    Some(Warning::DuplicateBasename { paths, .. }) => paths.push(path.to_path_buf()),
                    _ => self.report.warnings.push(Warning::DuplicateBasename {
                        stem: stem.to_owned(),
                        paths: vec![other, path.to_path_buf()],
                    }),
                }
            }
            self.by_stem.push((stem.to_owned(), path.to_path_buf()));
        }

        self.by_hash.push((source.sha256.clone(), path.to_path_buf()));
        self.report.sources.push(source);
    }
}

/// `<dir>/<stem>.<extension>`, stepped until it names nothing.
///
/// Two files in one folder can share a stem - `001.jpg` beside `001.webp` is
/// the ordinary way a folder ends up half-converted - and both would land on
/// `001.png`. The second becoming `001-2.png` keeps the pages distinct; the
/// stem warning §2 already raises is what tells the user the folder was
/// ambiguous, so nothing is being hidden by the rename.
fn free_path(dir: &Path, original: &Path, extension: &str) -> PathBuf {
    let stem = original.file_stem().and_then(|s| s.to_str()).unwrap_or("page");
    let mut candidate = dir.join(format!("{stem}.{extension}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}-{n}.{extension}"));
        n += 1;
    }
    candidate
}

/// A chapter must never publish a partially written page after a failed import.
fn write_import(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    crate::project::buffers::write_atomic(path, bytes).map_err(|e| e.to_string())
}

fn in_natural_order<T>(mut items: Vec<T>, name: impl Fn(&T) -> String) -> Vec<T> {
    items.sort_by(|a, b| natural_cmp(&name(a), &name(b)));
    items
}

/// Apply the whole of §2 to a list of candidate files, in one pass.
///
/// Takes the bytes rather than reading them, so the policy is testable without
/// a filesystem. **A caller ingesting a folder wants [`ingest_paths`]**: this
/// signature needs every file's bytes alive at once, which for a 200-page
/// chapter is exactly the peak memory use to avoid.
pub fn ingest<'a>(files: impl IntoIterator<Item = (&'a Path, &'a [u8])>) -> IngestReport {
    let mut sieve = Sieve::default();
    let candidates: Vec<(&Path, &[u8])> = files.into_iter().collect();
    for (path, bytes) in in_natural_order(candidates, |(path, _)| path.to_string_lossy().into_owned())
    {
        if is_junk(path) {
            sieve.skip(path, SkipReason::Junk);
            continue;
        }
        sieve.offer(path, bytes);
    }
    sieve.report
}

/// The whole of §2 over files on disk, holding **one file's bytes at a time**.
///
/// This is the shape a chapter is ingested through. `SourceRef` is header-only
/// precisely so that a 200-page chapter never has 200 pages in memory - but
/// that only holds if the bytes each header was read from are released
/// before the next file is opened,
/// which a `&[u8]` per file cannot express. So the read happens here, inside
/// the loop, and the buffer dies at the end of each iteration. Peak is one
/// page's file plus the one decode `decode_whole` performs on it.
///
/// The paths are collected - that is a list of names, not of pages - because
/// natural order has to be decided before the first file is opened.
pub fn ingest_paths(paths: impl IntoIterator<Item = PathBuf>) -> IngestReport {
    sift(paths, |path: &Path| std::fs::read(path), None)
}

/// [`ingest_paths`], taking every page into the job's own directory.
///
/// Every accepted file ends up inside `pages_dir` and **that file becomes the
/// chapter's page**: a PNG or TIFF is copied byte for byte, and a format this
/// application reads and cannot write is decoded and written as a lossless PNG -
/// TIFF for a CMYK source, which PNG has no colour type for. The user's file
/// is never modified, and after this it is never read again, which is what lets
/// the scan folder be moved or deleted.
///
/// This is the entry point a chapter is created through. `pages_dir` belongs to
/// the job, not to the user: writing beside the scans would put application
/// output in the input folder, which is the one thing forbidden outright.
///
/// The directory is created only when a file is actually accepted, so a folder
/// that yields no pages leaves no empty directory behind.
pub fn ingest_paths_importing(
    paths: impl IntoIterator<Item = PathBuf>,
    pages_dir: &Path,
) -> IngestReport {
    sift(paths, |path: &Path| std::fs::read(path), Some(pages_dir.to_path_buf()))
}

/// [`ingest_paths`] over an arbitrary reader.
///
/// The reader is a parameter so that "one file's bytes at a time" is a testable
/// claim rather than an assertion in a doc comment: a test can hand back a
/// buffer that says when it is dropped. `B: AsRef<[u8]>` rather than `Vec<u8>`
/// for the same reason.
///
/// A file that cannot be read is **listed** rather than dropped. Every
/// refused file must be "skipped and listed", and the catalogue's fixed
/// vocabulary has `input.skipReason.headerUnreadable` and no separate key
/// for an I/O failure -
/// which is the honest one of the six, since what failed is the read the header
/// would have come from. Inventing a key is a test failure.
pub fn ingest_with<B, F>(paths: impl IntoIterator<Item = PathBuf>, read: F) -> IngestReport
where
    B: AsRef<[u8]>,
    F: FnMut(&Path) -> std::io::Result<B>,
{
    sift(paths, read, None)
}

/// The loop both path-taking entry points are, with the conversion directory
/// as the only difference between them.
fn sift<B, F>(
    paths: impl IntoIterator<Item = PathBuf>,
    mut read: F,
    import_into: Option<PathBuf>,
) -> IngestReport
where
    B: AsRef<[u8]>,
    F: FnMut(&Path) -> std::io::Result<B>,
{
    let mut sieve = Sieve { import_into, ..Sieve::default() };
    let candidates: Vec<PathBuf> = paths.into_iter().collect();
    for path in in_natural_order(candidates, |path| path.to_string_lossy().into_owned()) {
        // Answered from the name, so a junk file is never opened.
        if is_junk(&path) {
            sieve.skip(&path, SkipReason::Junk);
            continue;
        }
        match read(&path) {
            Ok(bytes) => sieve.offer(&path, bytes.as_ref()),
            Err(error) => sieve.skip(&path, SkipReason::HeaderUnreadable(error.to_string())),
        }
    }
    sieve.report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{Format, encode, fixtures};

    fn png(name: &str) -> Vec<u8> {
        encode(&fixtures::by_name(name).raster, Format::Png).unwrap()
    }

    #[test]
    fn a_header_read_reports_mode_depth_and_profile_without_decoding() {
        let bytes = png("rgb16-icc");
        let source = source_ref(Path::new("003.png"), &bytes).unwrap();
        assert_eq!(source.mode, ColorMode::Rgb);
        assert_eq!(source.bit_depth, BitDepth::Sixteen);
        assert_eq!(source.icc_bytes, Some(fixtures::srgb_profile()));
        assert_eq!(source.sha256.len(), 64);
    }

    #[test]
    fn pages_come_back_in_natural_order() {
        let mut names = vec!["page10.png", "page2.png", "page1.png", "Page3.png"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["page1.png", "page2.png", "Page3.png", "page10.png"]);
    }

    #[test]
    fn case_and_zero_padding_only_break_ties() {
        use std::cmp::Ordering;
        // Case at position 0 must not outrank the digit at position 4.
        assert_eq!(natural_cmp("Page3", "page1"), Ordering::Greater);
        // Zero padding must not outrank the letter after the run.
        assert_eq!(natural_cmp("001a", "1b"), Ordering::Less);
        // With nothing else to separate them, the tiebreak decides.
        assert_eq!(natural_cmp("001", "1"), Ordering::Less);
        assert_eq!(natural_cmp("Page", "page"), Ordering::Less);
        assert_eq!(natural_cmp("page1", "page1"), Ordering::Equal);
    }

    #[test]
    fn junk_is_skipped_and_counted_rather_than_read() {
        assert!(is_junk(Path::new(".DS_Store")));
        assert!(is_junk(Path::new("Thumbs.db")));
        assert!(is_junk(Path::new("desktop.ini")));
        assert!(is_junk(Path::new("__MACOSX/001.png")));
        assert!(!is_junk(Path::new("001.png")));
    }

    #[test]
    fn a_truncated_file_is_skipped_rather_than_partly_cleaned() {
        let whole = png("rgb8");
        let truncated = &whole[..whole.len() / 2];
        let path = Path::new("003.png");
        // Strict metadata scanning may reject the truncated chunk before pixels.
        // Both boundaries must refuse the partial file.
        assert!(matches!(decode_whole(truncated), Err(SkipReason::PartialDecode(_))));

        let report = ingest([(path, truncated)]);
        assert!(report.sources.is_empty());
        assert!(matches!(report.skipped[0].reason, SkipReason::PartialDecode(_) | SkipReason::HeaderUnreadable(_)));
    }

    #[test]
    fn identical_bytes_are_ingested_once() {
        let bytes = png("l8");
        // Named so the natural order is unambiguous: `001.png` precedes
        // `001b.png`, so the copy is the one dropped.
        let report = ingest([
            (Path::new("001b.png"), bytes.as_slice()),
            (Path::new("001.png"), bytes.as_slice()),
        ]);
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].path, PathBuf::from("001.png"));
        assert_eq!(
            report.skipped[0].reason,
            SkipReason::Duplicate { of: PathBuf::from("001.png") }
        );
    }

    #[test]
    fn the_same_basename_under_two_extensions_warns_instead_of_guessing() {
        let a = png("l8");
        let b = png("rgb8");
        let report = ingest([
            (Path::new("001.png"), a.as_slice()),
            (Path::new("001.tif"), b.as_slice()),
        ]);
        assert_eq!(report.sources.len(), 2, "both are kept - the warning is not a rejection");
        assert!(matches!(&report.warnings[0], Warning::DuplicateBasename { stem, .. } if stem == "001"));
    }

    #[test]
    fn a_file_that_is_not_an_image_is_named_as_such() {
        let report = ingest([(Path::new("notes.txt"), b"hello".as_slice())]);
        assert_eq!(report.skipped[0].reason, SkipReason::NotAnImage);
        assert_eq!(report.skipped[0].reason.reason_key(), "input.skipReason.notAnImage");
    }

    /// A buffer that says when it is dropped, so a test can watch how many
    /// files' bytes are alive at once.
    struct Watched {
        bytes: Vec<u8>,
        live: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl AsRef<[u8]> for Watched {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            self.live.set(self.live.get() - 1);
        }
    }

    /// The whole reason for a header-only `SourceRef`: a 200-page chapter
    /// must never be 200 pages in memory. The property is not "the report is
    /// right" - `ingest` gets that right while holding every file at once - it
    /// is that each file's bytes are released before the next file is opened.
    #[test]
    fn a_folder_is_ingested_one_files_bytes_at_a_time() {
        let live = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let peak = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let pages: Vec<Vec<u8>> = (0..8u8)
            .map(|n| {
                let mut raster = fixtures::by_name("l8").raster;
                raster.data[0] = n;
                encode(&raster, Format::Png).unwrap()
            })
            .collect();

        let paths: Vec<PathBuf> =
            (1..=8).map(|n| PathBuf::from(format!("{n:03}.png"))).collect();
        let report = {
            let (live, peak, pages) = (live.clone(), peak.clone(), pages.clone());
            ingest_with(paths, move |path| {
                let n: usize = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
                live.set(live.get() + 1);
                peak.set(peak.get().max(live.get()));
                Ok(Watched { bytes: pages[n - 1].clone(), live: live.clone() })
            })
        };

        assert_eq!(report.sources.len(), 8);
        assert_eq!(peak.get(), 1, "{} files' bytes were alive at once", peak.get());
        assert_eq!(live.get(), 0, "a page's bytes outlived the ingest");
    }

    /// The lazy path is the same policy, not a second one: junk, duplicates,
    /// refusals and natural order all have to come out identical, or the
    /// library and the tests are testing different rules.
    #[test]
    fn reading_lazily_reaches_the_same_verdicts_as_reading_everything_first() {
        let l8 = png("l8");
        let rgb = png("rgb8");
        let files: Vec<(PathBuf, Vec<u8>)> = vec![
            (PathBuf::from("010.png"), rgb.clone()),
            (PathBuf::from("002.png"), l8.clone()),
            (PathBuf::from("002.tif"), l8.clone()),
            (PathBuf::from(".DS_Store"), b"junk".to_vec()),
            (PathBuf::from("notes.txt"), b"hello".to_vec()),
        ];

        let eager = ingest(files.iter().map(|(p, b)| (p.as_path(), b.as_slice())));
        let lazy = ingest_with(files.iter().map(|(p, _)| p.clone()), |path| {
            Ok::<_, std::io::Error>(
                files.iter().find(|(p, _)| p == path).map(|(_, b)| b.clone()).unwrap(),
            )
        });

        assert_eq!(
            lazy.sources.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
            eager.sources.iter().map(|s| s.path.clone()).collect::<Vec<_>>()
        );
        assert_eq!(lazy.skipped, eager.skipped);
        assert_eq!(lazy.warnings, eager.warnings);
    }

    /// §2: every refused file is "skipped and listed". A file the process
    /// cannot open is a refused file, and dropping it silently is how a
    /// twenty-four-file folder becomes a twenty-two-page chapter with nothing
    /// on record saying why.
    #[test]
    fn a_file_that_cannot_be_read_is_listed_rather_than_dropped() {
        let report = ingest_with([PathBuf::from("001.png"), PathBuf::from("002.png")], |path| {
            if path.file_stem().unwrap() == "002" {
                Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"))
            } else {
                Ok(png("l8"))
            }
        });
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.skipped.len(), 1, "the unreadable file vanished");
        assert_eq!(report.skipped[0].path, PathBuf::from("002.png"));
        assert_eq!(report.skipped[0].reason.reason_key(), "input.skipReason.headerUnreadable");
    }
    /// A scratch directory that removes itself, on the same terms as
    /// `project`'s: a unique path is wanted, not a dependency.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join("cleaner-core-ingest")
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

    /// A real JPEG of a fixture, through the same encoder `image::foreign`'s
    /// own tests use.
    fn jpeg(name: &str) -> Vec<u8> {
        let raster = &fixtures::by_name(name).raster;
        let buffer = image::RgbImage::from_raw(raster.width, raster.height, raster.data.clone())
            .expect("the fixture is 8-bit RGB");
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        bytes
    }

    /// The same fixture inverted and written as a BMP: a *different* image in
    /// a *different* foreign format, which is what the stem test needs so that
    /// neither file is refused as a duplicate of the other.
    fn bmp_unlike(name: &str) -> Vec<u8> {
        let raster = &fixtures::by_name(name).raster;
        let inverted: Vec<u8> = raster.data.iter().map(|b| !b).collect();
        let buffer = image::RgbImage::from_raw(raster.width, raster.height, inverted)
            .expect("the fixture is 8-bit RGB");
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Bmp)
            .unwrap();
        bytes
    }

    fn write(dir: &Scratch, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A non-native format becomes a PNG inside the chapter's page directory.
    #[test]
    fn a_foreign_page_becomes_a_png_the_pipeline_can_read() {
        let scratch = Scratch::new("convert");
        let original = write(&scratch, "001.bmp", &bmp_unlike("rgb8"));
        let into = scratch.join("chapter/pages");

        let report = ingest_paths_importing([original.clone()], &into);

        assert_eq!(report.skipped, vec![]);
        assert_eq!(report.sources.len(), 1);
        let source = &report.sources[0];
        assert_eq!(source.path, into.join("001.png"));
        assert_eq!(source.converted_from.as_deref(), Some(original.as_path()));
        // The original is where it was, byte for byte.
        assert!(original.exists());
        // And what was written really is a PNG this crate reads.
        let written = std::fs::read(&source.path).unwrap();
        assert_eq!(Format::sniff(&written), Some(Format::Png));
        assert_eq!(source.sha256, sha256_hex(&written));
        assert_eq!(decode_whole(&written).unwrap().width, source.width);

        assert_eq!(report.converted.len(), 1);
        assert_eq!(report.converted[0].from, "BMP");
        assert_eq!(report.converted[0].original, original);
    }

    /// The change the import made: a page in a format this application *can*
    /// write is still taken into the job's own directory, so the chapter stops
    /// needing the user's folder at all.
    #[test]
    fn a_native_page_is_copied_into_the_job_rather_than_read_where_it_lies() {
        let scratch = Scratch::new("import");
        let bytes = png("l8");
        let original = write(&scratch, "001.png", &bytes);
        let into = scratch.join("pages");

        let report = ingest_paths_importing([original.clone()], &into);

        assert_eq!(report.skipped, vec![]);
        assert_eq!(report.sources.len(), 1);
        let source = &report.sources[0];
        // The page is the library's file; the user's file is the origin.
        assert_eq!(source.path, into.join("001.png"));
        assert_eq!(source.converted_from.as_deref(), Some(original.as_path()));
        // Byte for byte, so the hash the header read still names the copy.
        assert_eq!(std::fs::read(&source.path).unwrap(), bytes);
        assert_eq!(source.sha256, sha256_hex(&bytes));
        // The user's file is untouched, and this is not a conversion: a notice
        // saying "1 file converted" about a PNG would be a lie.
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        assert_eq!(report.converted, vec![]);
    }

    #[test]
    fn jpeg_import_keeps_original_bytes_extension_and_hash() {
        let scratch = Scratch::new("jpeg-native");
        let bytes = jpeg("rgb8");
        let original = write(&scratch, "001.jpeg", &bytes);
        let into = scratch.join("chapter/pages");
        let report = ingest_paths_importing([original.clone()], &into);
        assert!(report.skipped.is_empty());
        assert!(report.converted.is_empty());
        let source = &report.sources[0];
        assert_eq!(source.path, into.join("001.jpeg"));
        assert_eq!(std::fs::read(&source.path).unwrap(), bytes);
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        assert_eq!(source.sha256, sha256_hex(&bytes));
        assert_eq!(source.converted_from, Some(original));
    }

    #[test]
    fn converted_formats_keep_decoded_samples_inside_the_chapter() {
        for (extension, format) in [
            ("webp", image::ImageFormat::WebP),
            ("gif", image::ImageFormat::Gif),
            ("bmp", image::ImageFormat::Bmp),
        ] {
            let scratch = Scratch::new(extension);
            let raster = fixtures::by_name("rgb8").raster;
            let buffer = image::RgbImage::from_raw(raster.width, raster.height, raster.data).unwrap();
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgb8(buffer).write_to(&mut std::io::Cursor::new(&mut bytes), format).unwrap();
            let expected = foreign::decode(&bytes).unwrap();
            let original = write(&scratch, &format!("001.{extension}"), &bytes);
            let into = scratch.join("chapter/pages");
            let report = ingest_paths_importing([original.clone()], &into);
            assert!(report.skipped.is_empty(), "{extension}: {:?}", report.skipped);
            assert_eq!(report.converted.len(), 1);
            assert_eq!(report.sources[0].path, into.join("001.png"));
            let saved = decode_whole(&std::fs::read(&report.sources[0].path).unwrap()).unwrap();
            assert_eq!(saved.data, expected.data, "{extension}: decoded colors changed");
            assert_eq!(saved.mode, expected.mode);
            assert_eq!(saved.depth, expected.depth);
            assert_eq!(saved.icc, expected.icc);
            assert_eq!(std::fs::read(original).unwrap(), bytes);
        }
    }

    #[test]
    fn converted_originals_survive_scan_deletion_with_verified_provenance() {
        let scratch = Scratch::new("archived-animation");
        let bytes = include_bytes!("../tests/fixtures/color-reference/animated.gif");
        let original = write(&scratch, "animated.gif", bytes);
        let dir = scratch.join("chapter/pages");
        let report = ingest_paths_importing([original.clone()], &dir);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let source = &report.sources[0];
        let provenance = source.conversion.as_ref().unwrap();
        assert_eq!(provenance.frame_count, 2);
        assert_eq!(provenance.chosen_frame, 0);
        assert!(provenance.archived_original.starts_with(scratch.join("chapter/originals")));
        assert_eq!(std::fs::read(&provenance.archived_original).unwrap(), bytes);
        assert_eq!(provenance.original_sha256, sha256_hex(bytes));
        assert_eq!(provenance.working_sha256, source.sha256);
        std::fs::remove_file(original).unwrap();
        assert!(decode_whole(&std::fs::read(&source.path).unwrap()).is_ok());
        let project = crate::project::Project::new(&scratch.join("chapter"), "test", crate::project::StripMode::Single, &report.sources);
        assert!(!project.sources[0].conversion.as_ref().unwrap().archived_original.is_absolute());
        let reopened: crate::project::Project = serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
        assert_eq!(reopened.sources[0].conversion, project.sources[0].conversion);
    }

    #[test]
    fn invalid_tagged_native_jpeg_is_retained_but_reencoding_refuses_it() {
        use image::ImageEncoder;
        let scratch = Scratch::new("invalid-native-icc");
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new(&mut bytes);
        encoder.set_icc_profile(b"invalid ICC preserved with original".to_vec()).unwrap();
        encoder.encode(&[10, 20, 30], 1, 1, image::ExtendedColorType::Rgb8).unwrap();
        let path = write(&scratch, "native.jpg", &bytes);
        let report = ingest_paths_importing([path], &scratch.join("chapter/pages"));
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(std::fs::read(&report.sources[0].path).unwrap(), bytes);
        let copy = crate::export::export_page(&bytes, &[], crate::export::Target::SameAsSource).unwrap();
        assert_eq!(copy.bytes, bytes);
        assert!(crate::export::export_page(&bytes, &[], crate::export::Target::Explicit(Format::Png)).is_err());
    }

    #[test]
    fn webp_conversion_preserves_exif_orientation_and_owns_the_archive() {
        use image::ImageEncoder;
        let scratch = Scratch::new("webp-exif");
        let mut exif = b"II\x2a\0\x08\0\0\0\x01\0".to_vec();
        exif.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut bytes);
        encoder.set_exif_metadata(exif).unwrap();
        encoder.encode(&[255, 0, 0, 0, 0, 255], 2, 1, image::ExtendedColorType::Rgb8).unwrap();
        let original = write(&scratch, "001.webp", &bytes);
        let report = ingest_paths_importing([original], &scratch.join("chapter/pages"));
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let source = &report.sources[0];
        let working = std::fs::read(&source.path).unwrap();
        assert_eq!(crate::image::orientation::from_bytes(&working).0, 6);
        assert_eq!(std::fs::read(&source.conversion.as_ref().unwrap().archived_original).unwrap(), bytes);
        assert!(source.conversion.as_ref().unwrap().retained.iter().any(|value| value.contains("EXIF")));
    }

    #[test]
    fn failed_working_page_write_removes_new_archive() {
        let scratch = Scratch::new("archive-cleanup");
        let original = write(&scratch, "001.bmp", &bmp_unlike("rgb8"));
        let dir = scratch.join("chapter/pages");
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
        let report = ingest_paths_importing([original], &dir);
        assert!(report.sources.is_empty());
        assert!(std::fs::read_dir(scratch.join("chapter/originals")).unwrap().next().is_none());
    }

    /// The import is the *last* step of accepting a page, so everything §2
    /// refuses is refused on bytes alone and costs the library nothing.
    #[test]
    fn a_page_the_sieve_refuses_leaves_no_copy_behind() {
        let scratch = Scratch::new("import-refused");
        let bytes = png("l8");
        let first = write(&scratch, "001.png", &bytes);
        write(&scratch, "002.png", &bytes);
        let whole = png("l8");
        write(&scratch, "003.png", &whole[..whole.len() / 3]);
        let into = scratch.join("pages");

        let report = ingest_paths_importing(read_dir(&scratch), &into);

        assert_eq!(report.sources.len(), 1);
        assert!(
            report.skipped.iter().any(|s| matches!(&s.reason, SkipReason::Duplicate { of } if *of == first))
        );
        assert!(report.skipped.iter().any(|s| matches!(s.reason, SkipReason::PartialDecode(_))));
        assert_eq!(std::fs::read_dir(&into).unwrap().count(), 1, "a refused page was copied");
    }

    /// The library's own write failing says nothing about the user's file, so
    /// it is a separate refusal with a separate sentence.
    #[test]
    fn a_copy_that_cannot_be_written_is_listed_as_an_import_failure() {
        let scratch = Scratch::new("import-fails");
        let original = write(&scratch, "001.png", &png("l8"));
        // A file where the directory has to go: `create_dir_all` fails, and so
        // does every write under it.
        let into = scratch.join("pages");
        std::fs::write(&into, b"not a directory").unwrap();

        let report = ingest_paths_importing([original], &into);

        assert!(report.sources.is_empty());
        assert!(matches!(report.skipped[0].reason, SkipReason::ImportFailed(_)));
        assert_eq!(report.skipped[0].reason.reason_key(), "input.skipReason.importFailed");
    }

    /// JPEG is native even without an import directory. BMP still needs one.
    #[test]
    fn a_foreign_page_is_still_refused_when_nothing_asked_for_a_conversion() {
        let report = ingest([(Path::new("001.jpg"), jpeg("rgb8").as_slice())]);
        assert_eq!(report.sources[0].path, PathBuf::from("001.jpg"));
        assert!(report.converted.is_empty());
        let report = ingest([(Path::new("001.bmp"), bmp_unlike("rgb8").as_slice())]);
        assert!(report.sources.is_empty());
        assert_eq!(report.skipped[0].reason, SkipReason::NotAnImage);

        let report = ingest([(Path::new("001.png"), png("l8").as_slice())]);
        assert_eq!(report.sources[0].path, PathBuf::from("001.png"));
        assert_eq!(report.sources[0].converted_from, None);
    }

    /// The duplicate check happens **before** the write, so the second copy
    /// leaves no file behind.
    #[test]
    fn the_same_jpeg_twice_imports_once_without_conversion() {
        let scratch = Scratch::new("duplicate");
        let bytes = jpeg("rgb8");
        let first = write(&scratch, "001.jpg", &bytes);
        write(&scratch, "001b.jpg", &bytes);
        let into = scratch.join("converted");

        let report = ingest_paths_importing(read_dir(&scratch), &into);

        assert_eq!(report.sources.len(), 1);
        assert!(report.converted.is_empty());
        assert_eq!(std::fs::read(&report.sources[0].path).unwrap(), bytes);
        assert!(matches!(report.skipped[0].reason, SkipReason::Duplicate { ref of } if *of == first));
        assert_eq!(std::fs::read_dir(&into).unwrap().count(), 1, "a stray PNG was left behind");
    }

    /// `001.bmp` and `001.png` both want to be `001.png`. Both pages survive,
    /// and §2's stem warning still fires - the rename hides nothing.
    #[test]
    fn two_sources_with_one_stem_get_two_files() {
        let scratch = Scratch::new("stem");
        // Two different images, so neither is refused as a duplicate.
        write(&scratch, "001.png", &png("rgb8"));
        write(&scratch, "001.bmp", &bmp_unlike("rgb8"));
        let into = scratch.join("converted");

        let report = ingest_paths_importing(read_dir(&scratch), &into);

        assert_eq!(report.sources.len(), 2, "{:?}", report.skipped);
        let names: Vec<_> = report.sources.iter().map(|s| s.path.clone()).collect();
        assert!(names.contains(&into.join("001.png")));
        assert!(names.contains(&into.join("001-2.png")));
        assert!(matches!(report.warnings[0], Warning::DuplicateBasename { .. }));
    }

    /// A half-downloaded JPEG is the same fact as a truncated PNG, and gets
    /// the same answer rather than a page with a grey tail on it.
    #[test]
    fn a_truncated_foreign_page_is_skipped_rather_than_partly_converted() {
        let scratch = Scratch::new("truncated");
        let whole = jpeg("rgb8");
        write(&scratch, "001.jpg", &whole[..whole.len() / 3]);
        let into = scratch.join("converted");

        let report = ingest_paths_importing(read_dir(&scratch), &into);

        assert!(report.sources.is_empty());
        assert!(matches!(report.skipped[0].reason,
            SkipReason::HeaderUnreadable(_) | SkipReason::PartialDecode(_)));
        assert!(!into.exists(), "nothing to convert, so no directory");
    }

    fn read_dir(scratch: &Scratch) -> Vec<PathBuf> {
        std::fs::read_dir(&scratch.0)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect()
    }
}
