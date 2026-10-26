//! `planExportChapter` and `exportChapter`.
//!
//! **The plan reads headers and the manifest, never pixels.** It decides the
//! container, the destination, every refusal, each output's name and format,
//! and what the export must declare, and it stamps all of that with a
//! [`revision`]. The export plans again (cheap), checks the revision, writes
//! every file into a hidden staging folder beside the destination, checks the
//! revision once more and only then moves the files in. A page that fails to
//! decode part way publishes nothing.
//!
//! Every "no" is `status: 'refused'` with a `reasonKey` the catalogue has a
//! sentence for. Written: PNG and TIFF per page (with an optional
//! `<page>_mask.png` beside each), CBZ (those per-page files in one stored
//! zip), one stitched file for a longstrip chapter, and PSD per page (layered
//! or flattened). Lossy formats and PSB are refused by name; output never
//! overwrites input ([`Job::output_refusal`]).
//!
//! `pages` picks positions in `strip.order`, the interface's page index; none
//! means every page. A stitched file is always the whole strip.
//!
//! `grayscale` writes a grey page stored as RGB or RGBA as grayscale
//! ([`Tone`]); by default every page keeps its own colour type.

use std::path::{Path, PathBuf};

use cleaner_core::export::{
    Cbz, ExportError, PageSource, StripPatch, Target, Tone, export_mask_to,
    export_page_psd_to, export_page_to, export_stitched, plan_page, plan_stitched,
    preflight_page_psd, psd, psd_declarations,
};
#[cfg(test)]
use cleaner_core::export::patches_on_page;
use cleaner_core::image::{Format, Header, Raster, decode, png_header, tiff_header};
use cleaner_core::mask::Mask;
#[cfg(test)]
use cleaner_core::mask::Rect;
use cleaner_core::patch::Patch;
#[cfg(test)]
use cleaner_core::project::PatchRecord;
use cleaner_core::project::{Job, ProjectSource, StripMode, default_output_dir};
use cleaner_core::strip::Strip;

use crate::library;

/// The seam's shape for both commands, serialised as is.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_key: Option<&'static str>,
    /// Pixels of a stitched file no source page had. `None` for per-page
    /// output, which has no gutter at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gutter_pixels: Option<u64>,
    pub actual_formats: Vec<String>,
    pub declared: Vec<ExportDeclaration>,
    pub outputs: Vec<PlannedOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ExportResult {
    fn with_status(status: &'static str, reason_key: Option<&'static str>, path: Option<String>) -> ExportResult {
        ExportResult {
            status,
            file_count: None,
            path,
            reason_key,
            gutter_pixels: None,
            actual_formats: Vec::new(),
            declared: Vec::new(),
            outputs: Vec::new(),
            revision: None,
            detail: None,
        }
    }

    fn refused(reason_key: &'static str, path: Option<String>) -> ExportResult {
        ExportResult::with_status("refused", Some(reason_key), path)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDeclaration {
    pub page: usize,
    pub key: &'static str,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedOutput {
    pub page: usize,
    pub filename: String,
    pub format: String,
}

fn declarations(page: usize, values: &[cleaner_core::export::Declaration]) -> Vec<ExportDeclaration> {
    use cleaner_core::export::Declaration;
    values.iter().map(|value| ExportDeclaration {
        page, key: value.reason_key(), params: match value {
            Declaration::FormatChanged { requested, used, mode } => serde_json::json!({
                "page": page + 1, "requested": format_label(*requested), "used": format_label(*used), "mode": format!("{mode:?}") }),
            Declaration::MetadataNormalized { field, reason } => serde_json::json!({ "page": page + 1, "field": field, "reason": reason }),
            Declaration::AssumedInterpretation { interpretation } => serde_json::json!({ "page": page + 1, "interpretation": interpretation }),
        },
    }).collect()
}

fn format_label(format: Format) -> &'static str {
    match format { Format::Png => "PNG", Format::Tiff => "TIFF", Format::Jpeg => "JPEG" }
}

/// What the caller asked for, with the seam's defaults filled in.
#[derive(Debug, Clone, Copy)]
struct Choices<'a> {
    format: &'a str,
    destination: &'a str,
    masks: &'a str,
    layout: &'a str,
    pages: Option<&'a [usize]>,
    tone: Tone,
    /// Settings > Export, read when the command arrives, so a plan made
    /// under other PSD choices is stale.
    psd: psd::PsdOptions,
}

impl<'a> Choices<'a> {
    fn from_seam(
        format: &'a Option<String>,
        destination: &'a Option<String>,
        masks: &'a Option<String>,
        layout: &'a Option<String>,
        pages: &'a Option<Vec<usize>>,
        grayscale: Option<bool>,
        settings: &serde_json::Value,
    ) -> Choices<'a> {
        Choices {
            format: format.as_deref().unwrap_or("PNG"),
            destination: destination.as_deref().unwrap_or("new-folder"),
            masks: masks.as_deref().unwrap_or("flattened"),
            layout: layout.as_deref().unwrap_or("per-page"),
            pages: pages.as_deref(),
            tone: if grayscale.unwrap_or(false) { Tone::Grayscale } else { Tone::Source },
            psd: crate::settings::psd_options(settings),
        }
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn plan_export_chapter(
    app: tauri::AppHandle, chapter_id: String, format: Option<String>,
    destination: Option<String>, masks: Option<String>, layout: Option<String>,
    pages: Option<Vec<usize>>, grayscale: Option<bool>,
) -> Result<ExportResult, String> {
    library::blocking(move || {
        let path = library::resolve_chapter(&app, &chapter_id).map_err(|e| e.to_string())?;
        let _lock = crate::run::lock_job(&path)?;
        let job = Job::open(&path).map_err(|e| e.to_string())?;
        let settings = crate::settings::read(&app).unwrap_or(serde_json::Value::Null);
        let choices = Choices::from_seam(&format, &destination, &masks, &layout, &pages, grayscale, &settings);
        Ok(plan(&job, &choices)?.into_result())
    }).await
}

/// On the blocking executor, so the dialog's spinner can paint. Refused while
/// the chapter has a run: an export taken between a run's pages would publish
/// a half-cleaned chapter.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn export_chapter(
    app: tauri::AppHandle,
    chapter_id: String,
    format: Option<String>,
    destination: Option<String>,
    masks: Option<String>,
    layout: Option<String>,
    pages: Option<Vec<usize>>,
    grayscale: Option<bool>,
    plan_revision: Option<String>,
) -> Result<ExportResult, String> {
    let declared = format.clone().unwrap_or_else(|| "PNG".into());
    let result = library::blocking(move || {
        let job_path = library::resolve_chapter(&app, &chapter_id).map_err(|e| e.to_string())?;
        let _lock = crate::run::lock_chapter(&job_path)?;
        let job = Job::open(&job_path).map_err(|e| e.to_string())?;
        let settings = crate::settings::read(&app).unwrap_or(serde_json::Value::Null);
        let choices = Choices::from_seam(&format, &destination, &masks, &layout, &pages, grayscale, &settings);
        export_reviewed_job(&job, &choices, plan_revision.as_deref())
    })
    .await?;

    // The backend announces the outcome; the interface does not repeat it.
    if let Some((key, params, tone)) = announcement(&result, &declared) {
        crate::events::notice(key, params, tone);
    }
    Ok(result)
}

/// What an export result says on the notice stack, if anything. Statuses are
/// matched by name, so a status added later announces nothing until it is
/// given a sentence.
fn announcement(
    result: &ExportResult,
    declared: &str,
) -> Option<(&'static str, serde_json::Value, &'static str)> {
    let path = result.path.clone().unwrap_or_default();
    let actual = if result.actual_formats.is_empty() { declared.to_owned() } else { result.actual_formats.join(" / ") };
    match result.status {
        "refused" => result
            .reason_key
            .map(|key| (key, serde_json::json!({ "path": path, "format": declared }), "warn")),
        "exported" => Some(match result.gutter_pixels {
            Some(gutter) => (
                "notice.export.stitched",
                serde_json::json!({ "count": gutter, "format": actual, "path": path }),
                "info",
            ),
            None => (
                "notice.export.finished",
                serde_json::json!({
                    "count": result.file_count.unwrap_or(0),
                    "format": actual,
                    "path": path,
                }),
                "info",
            ),
        }),
        "partial" => Some(("notice.export.partial", serde_json::json!({ "path": path, "detail": result.detail, "count": result.file_count.unwrap_or(0) }), "warn")),
        _ => None,
    }
}

/* ------------------------------------------------------------------ */
/* Plan                                                               */
/* ------------------------------------------------------------------ */

/// An export that can go ahead: where, in what shape, and the seam's answer.
struct Plan {
    container: Container,
    dir: PathBuf,
    result: ExportResult,
}

enum Planned {
    Ready(Plan),
    Refused(ExportResult),
}

impl Planned {
    fn into_result(self) -> ExportResult {
        match self {
            Planned::Ready(plan) => plan.result,
            Planned::Refused(result) => result,
        }
    }
}

/// Why planning one output stopped.
enum Stop {
    /// A refusal the catalogue has its own sentence for.
    Refused(&'static str),
    /// A page that cannot be read or represented.
    Invalid(String),
}

fn refuse(reason_key: &'static str, path: Option<&Path>) -> Result<Planned, String> {
    Ok(Planned::Refused(ExportResult::refused(reason_key, path.map(|p| p.display().to_string()))))
}

/// Everything an export will do, decided before anything is created.
fn plan(job: &Job, choices: &Choices) -> Result<Planned, String> {
    let requested = match requested_format(choices.format) {
        Ok(requested) => requested,
        Err(key) => return refuse(key, None),
    };
    let order = &job.project.strip.order;
    let longstrip = job.project.strip.mode == StripMode::Longstrip;
    let container = match container_for(requested, choices.layout, mask_files(choices.masks), longstrip) {
        Ok(container) => container,
        Err(key) => return refuse(key, None),
    };
    let Some(positions) = selected_pages(choices.pages, order.len()) else {
        return refuse("notice.export.refusedPageSelection", None);
    };
    // A stitched file and a CBZ are one file for the whole chapter: one made
    // of some pages would replace the whole chapter's file under its name.
    if matches!(container, Container::Stitched { .. } | Container::Archive) && positions.len() != order.len() {
        return refuse("notice.export.refusedPageSelection", None);
    }
    let dir = match destination_dir(choices.destination, &source_directory(job)) {
        Ok(dir) => dir,
        Err(key) => return refuse(key, None),
    };
    if let Some(refusal) = job.output_refusal(&dir) {
        // The notice names the directory it would have replaced.
        return refuse(refusal.reason_key(), Some(&dir));
    }
    if !destination_available(&dir) {
        return refuse("notice.export.destinationUnavailable", Some(&dir));
    }

    let mut result = ExportResult::with_status("planned", None, Some(dir.display().to_string()));
    let checked = match container {
        Container::Stitched { target, .. } => plan_strip(job, target, choices.tone, &mut result),
        _ => positions.iter().try_for_each(|&position| plan_output(job, container, position, choices, &mut result)),
    };
    if let Err(stop) = checked {
        let (key, detail) = match stop {
            Stop::Refused(key) => (key, None),
            Stop::Invalid(detail) => ("notice.export.invalidSource", Some(detail)),
        };
        let mut refused = ExportResult::refused(key, result.path);
        refused.detail = detail;
        return Ok(Planned::Refused(refused));
    }

    let mut names = std::collections::HashSet::new();
    let mut output_names: Vec<(usize, String)> =
        result.outputs.iter().map(|output| (output.page, output.filename.clone())).collect();
    if matches!(container, Container::Pages { masks: MaskFiles::Beside, .. }) {
        output_names.extend(positions.iter().filter_map(|&position| {
            job.project.sources.get(order[position]).map(|source| (position, mask_filename(source)))
        }));
    }
    for (page, filename) in output_names {
        if !names.insert(filename.to_lowercase()) {
            let mut refused = ExportResult::refused("notice.export.invalidSource", result.path);
            refused.detail = Some(format!("Page {} would overwrite another output named {filename}; rename the conflicting source page before exporting", page + 1));
            return Ok(Planned::Refused(refused));
        }
    }
    result.actual_formats = result.outputs.iter().map(|output| output.format.clone()).collect();
    result.actual_formats.sort();
    result.actual_formats.dedup();
    result.file_count = Some(result.outputs.len() as u32);
    result.revision = Some(revision(job, choices)?);
    Ok(Planned::Ready(Plan { container, dir, result }))
}

/// The strip positions an export covers, sorted and once each: every page
/// when none were named, `None` when a named one is not in the chapter.
fn selected_pages(pages: Option<&[usize]>, count: usize) -> Option<Vec<usize>> {
    let Some(pages) = pages else { return Some((0..count).collect()) };
    let mut positions = pages.to_vec();
    positions.sort_unstable();
    positions.dedup();
    (!positions.is_empty() && positions.iter().all(|&position| position < count)).then_some(positions)
}

/// One stitched file, from page headers.
fn plan_strip(job: &Job, target: Target, tone: Tone, result: &mut ExportResult) -> Result<(), Stop> {
    let applied = job.project.patches.iter().any(|record| record.visible);
    let planned = plan_stitched(&strip_of(job), &mut JobPages { job }, target, applied, tone).map_err(|error| match error {
        ExportError::NotStitchable(refusal) => Stop::Refused(refusal.reason_key()),
        other => Stop::Invalid(other.to_string()),
    })?;
    result.declared.extend(declarations(0, &planned.declared));
    result.outputs.push(PlannedOutput {
        page: 0,
        filename: format!("{}.{}", chapter_stem(job), extension(planned.format)),
        format: format_label(planned.format).into(),
    });
    result.gutter_pixels = Some(planned.gutter_pixels);
    Ok(())
}

/// One page's file, from its header and its patches.
fn plan_output(
    job: &Job,
    container: Container,
    position: usize,
    choices: &Choices,
    result: &mut ExportResult,
) -> Result<(), Stop> {
    let invalid = |error: String| Stop::Invalid(format!("page {}: {error}", position + 1));
    let (source, bytes, patches) = page_inputs(job, position).map_err(invalid)?;
    let (format, declared) = match container {
        Container::Layered { layered } => {
            if let Some(refusal) = psd::refusal(source.w, source.h, source.mode, source.bit_depth) {
                return Err(Stop::Refused(refusal.reason_key()));
            }
            preflight_page_psd(&bytes, &patches, layered, &choices.psd).map_err(|error| match error {
                ExportError::NotLayerable(refusal) => Stop::Refused(refusal.reason_key()),
                other => invalid(other.to_string()),
            })?;
            ("PSD", psd_declarations(&bytes, &patches, choices.tone).map_err(|e| invalid(e.to_string()))?)
        }
        _ => {
            let target = match container { Container::Pages { target, .. } => target, _ => Target::SameAsSource };
            let planned = plan_page(&bytes, &patches, target, choices.tone).map_err(|e| invalid(e.to_string()))?;
            (format_label(planned.format), planned.declared)
        }
    };
    result.declared.extend(declarations(position, &declared));
    let ext = if format == "JPEG" { "jpg".to_owned() } else { format.to_lowercase() };
    result.outputs.push(PlannedOutput { page: position, filename: output_filename(source, &ext), format: format.to_owned() });
    Ok(())
}

/// Moves whenever anything the export reads moves: the manifest, the choices,
/// and the size or modification time of every source and patch file. Stat
/// calls only, so planning stays cheap on a long chapter.
fn revision(job: &Job, choices: &Choices) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(&job.project).map_err(|e| e.to_string())?);
    hash.update(std::fs::read(job.path()).map_err(|e| e.to_string())?);
    hash.update(format!("{choices:?}"));
    let mut stamp = |path: &Path| {
        let meta = std::fs::metadata(path).ok();
        let len = meta.as_ref().map(std::fs::Metadata::len);
        let modified = meta.and_then(|meta| meta.modified().ok());
        hash.update(format!("{}|{len:?}|{modified:?}\n", path.display()));
    };
    for &index in &job.project.strip.order {
        stamp(&job.source_path(index).ok_or("missing source")?);
    }
    for record in &job.project.patches {
        let mut refs = vec![record.buffer_ref.clone(), record.mask_ref.clone(), record.ink_ref()];
        for part in &record.native_parts {
            refs.extend([part.buffer_ref.clone(), part.mask_ref.clone(), part.ink_ref.clone()]);
        }
        for name in refs {
            stamp(&job.sidecar().join(name));
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

/* ------------------------------------------------------------------ */
/* Export                                                             */
/* ------------------------------------------------------------------ */

/// The command's body, without the `AppHandle`.
#[cfg(test)]
fn export_open_job(job: &Job, format: &str, destination: &str, masks: &str, layout: &str) -> Result<ExportResult, String> {
    let choices = Choices { format, destination, masks, layout, pages: None, tone: Tone::Source, psd: Default::default() };
    export_reviewed_job(job, &choices, None)
}

fn export_reviewed_job(job: &Job, choices: &Choices, expected: Option<&str>) -> Result<ExportResult, String> {
    let plan = match plan(job, choices)? {
        Planned::Ready(plan) => plan,
        Planned::Refused(result) => return Ok(result),
    };
    if expected.is_some_and(|expected| plan.result.revision.as_deref() != Some(expected)) {
        return Ok(ExportResult::refused("notice.export.stalePlan", None));
    }
    std::fs::create_dir_all(&plan.dir).map_err(|e| format!("{}: {e}", plan.dir.display()))?;
    let dir = plan.dir.canonicalize().map_err(|e| format!("{}: {e}", plan.dir.display()))?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stage = dir.join(format!(".export-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::create_dir(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    let outcome = publish(job, choices, plan, &dir, &stage);
    let _ = std::fs::remove_dir_all(&stage);
    outcome
}

/// Write into `stage`, check nothing changed underneath, then move each file
/// into `dir`.
fn publish(job: &Job, choices: &Choices, plan: Plan, dir: &Path, stage: &Path) -> Result<ExportResult, String> {
    if let Err(error) = write(job, &plan, choices, stage) {
        let mut result = ExportResult::with_status("partial", Some("notice.export.partial"), Some(dir.display().to_string()));
        result.file_count = Some(0);
        result.detail = Some(format!("No files published: {error}"));
        return Ok(result);
    }
    // External edits can bypass the in-process job lock.
    if Some(revision(job, choices)?) != plan.result.revision {
        return Ok(ExportResult::refused("notice.export.stalePlan", None));
    }
    let mut files = std::fs::read_dir(stage).map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.path()).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>()?;
    files.sort();
    let mut result = plan.result;
    result.status = "exported";
    result.path = Some(match plan.container {
        Container::Archive => dir.join(format!("{}.cbz", chapter_stem(job))),
        Container::Stitched { .. } => dir.join(&result.outputs[0].filename),
        _ => dir.to_path_buf(),
    }.display().to_string());
    let mut completed = Vec::new();
    for file in files {
        let target = dir.join(file.file_name().ok_or("missing output filename")?);
        if let Err(error) = std::fs::rename(&file, &target) {
            result.status = "partial";
            result.reason_key = Some("notice.export.partial");
            result.file_count = Some(completed.len() as u32);
            result.detail = Some(format!("{}: {error}; completed: {}", target.display(), completed.join(", ")));
            result.outputs.retain(|output| completed.iter().any(|path: &String| Path::new(path).file_name().is_some_and(|name| name == output.filename.as_str())));
            result.actual_formats = result.outputs.iter().map(|output| output.format.clone()).collect();
            result.actual_formats.sort();
            result.actual_formats.dedup();
            break;
        }
        completed.push(target.display().to_string());
    }
    Ok(result)
}

/// Every file the plan names, into `stage`.
fn write(job: &Job, plan: &Plan, choices: &Choices, stage: &Path) -> Result<(), String> {
    let outputs = &plan.result.outputs;
    let tone = choices.tone;
    match plan.container {
        Container::Pages { target, masks } => {
            for output in outputs {
                let (source, bytes, patches) = page_inputs(job, output.page)?;
                let summary = write_file(&stage.join(&output.filename), |file| {
                    export_page_to(&bytes, &patches, target, tone, file).map_err(|e| format!("{}: {e}", output.filename))
                })?;
                debug_assert_eq!(format_label(summary.format), output.format, "the file was named for another format");
                if masks == MaskFiles::Beside {
                    let orientation = cleaner_core::image::orientation::from_bytes(&bytes);
                    let (width, height) = orientation.size(source.w, source.h);
                    let oriented: Vec<Patch> = patches.iter().map(|p| orientation.patch(p, source.w, source.h)).collect();
                    write_mask_file(&stage.join(mask_filename(source)), &oriented, width, height)?;
                }
            }
            Ok(())
        }
        Container::Layered { layered } => {
            for output in outputs {
                let (_, bytes, patches) = page_inputs(job, output.page)?;
                write_file(&stage.join(&output.filename), |file| {
                    export_page_psd_to(&bytes, &patches, layered, tone, &choices.psd, file).map_err(|e| format!("{}: {e}", output.filename))
                })?;
            }
            Ok(())
        }
        Container::Archive => write_archive(job, outputs, tone, &stage.join(format!("{}.cbz", chapter_stem(job)))),
        Container::Stitched { target, masks } => write_stitched(job, target, tone, masks, &stage.join(&outputs[0].filename), stage),
    }
}

/// One page's source record, file bytes and patches, by strip position.
fn page_inputs(job: &Job, position: usize) -> Result<(&ProjectSource, Vec<u8>, Vec<Patch>), String> {
    let missing = || format!("page {}: missing source", position + 1);
    let index = *job.project.strip.order.get(position).ok_or_else(missing)?;
    let source = job.project.sources.get(index).ok_or_else(missing)?;
    let path = job.source_path(index).ok_or_else(missing)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let patches = page_patches_for(job, position)?;
    Ok((source, bytes, patches))
}

/// The resolved format, layout and mask choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    /// One file per page, with or without a mask file beside each.
    Pages { target: Target, masks: MaskFiles },
    /// One `.cbz` holding one file per page.
    Archive,
    /// One file for the whole strip, with or without its mask file.
    Stitched { target: Target, masks: MaskFiles },
    /// One `.psd` per page: layered, or one flattened layer.
    Layered { layered: bool },
}

/// Whether a raster export writes `001_mask.png` beside `001.png`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaskFiles {
    None,
    Beside,
}

fn requested_format(format: &str) -> Result<Requested, &'static str> {
    match format.to_ascii_uppercase().as_str() {
        "PNG" => Ok(Requested::Png),
        "TIFF" | "TIF" => Ok(Requested::Tiff),
        "CBZ" => Ok(Requested::Cbz),
        "PSD" => Ok(Requested::Psd),
        // Lossy formats need an acknowledgement flow that does not exist.
        // Lossless WebP is refused on the same line.
        "JPEG" | "JPG" | "WEBP" => Err("notice.export.refusedLossyFormat"),
        "PSB" => Err("notice.export.refusedLayeredFormat"),
        _ => Err("notice.export.refusedUnknownFormat"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Requested {
    Png,
    Tiff,
    Cbz,
    Psd,
}

/// An unrecognised layout is per-page, which every chapter can do.
fn container_for(
    requested: Requested,
    layout: &str,
    masks: MaskFiles,
    longstrip: bool,
) -> Result<Container, &'static str> {
    let stitched = layout == "stitched";
    if requested == Requested::Psd {
        // A PSD holds its whole page as the Background, and the strip is never
        // resident whole.
        if stitched {
            return Err("notice.export.refusedStitchedLayered");
        }
        return Ok(Container::Layered { layered: masks == MaskFiles::Beside });
    }
    let target = match requested {
        Requested::Png => Target::Explicit(Format::Png),
        Requested::Tiff => Target::Explicit(Format::Tiff),
        // A CBZ's pages keep their own lossless format, so an untouched page
        // goes in byte for byte.
        Requested::Cbz | Requested::Psd => Target::SameAsSource,
    };
    if !stitched {
        return match requested {
            // A reader app would show a mask file inside the archive as a page.
            Requested::Cbz if masks == MaskFiles::Beside => Err("notice.export.refusedMaskLayers"),
            Requested::Cbz => Ok(Container::Archive),
            _ => Ok(Container::Pages { target, masks }),
        };
    }
    if requested == Requested::Cbz {
        return Err("notice.export.refusedStitchedArchive");
    }
    if !longstrip {
        return Err("notice.export.refusedStitchPaginated");
    }
    Ok(Container::Stitched { target, masks })
}

/// `'separate-layer'` asks for masks as their own thing; anything else is the
/// image alone.
fn mask_files(masks: &str) -> MaskFiles {
    match masks {
        "separate-layer" => MaskFiles::Beside,
        _ => MaskFiles::None,
    }
}

/// Two sentinels or an absolute path. A relative path has nothing both sides
/// of the seam would agree it is relative to, so it is refused.
fn destination_dir(destination: &str, source_dir: &Path) -> Result<PathBuf, &'static str> {
    match destination {
        "new-folder" => Ok(default_output_dir(source_dir)),
        "source-folder" => Ok(source_dir.to_path_buf()),
        other if Path::new(other).is_absolute() => Ok(PathBuf::from(other)),
        _ => Err("notice.export.refusedDestination"),
    }
}

/// Whether `dir` could be created and written, without writing: the nearest
/// existing ancestor must be a writable directory. A chapter imported from a
/// drive that is now unplugged otherwise fails with a bare `Permission
/// denied` from inside `/Volumes`.
fn destination_available(dir: &Path) -> bool {
    dir.ancestors().find(|path| path.exists()).is_some_and(|existing| existing.is_dir() && writable(existing))
}

/// `access(2)`, not the permission bits, so a read-only mount answers too.
#[cfg(unix)]
fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    // SAFETY: `name` is a NUL-terminated string that lives across the call.
    unsafe { libc::access(name.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// Windows decides this from an ACL; the write itself reports the rest.
#[cfg(not(unix))]
fn writable(_dir: &Path) -> bool {
    true
}

fn write_mask_file(path: &Path, patches: &[Patch], width: u32, height: u32) -> Result<(), String> {
    let masks: Vec<&Mask> = patches.iter().filter(|p| p.visible).map(Patch::lettering).collect();
    write_file(path, |file| {
        export_mask_to(&masks, width, height, file).map_err(|e| format!("{}: {e}", path.display()))
    })
    .map(|_| ())
}

/// Create `path`, run `write` into it and flush.
fn write_file<T>(
    path: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<T, String>,
) -> Result<T, String> {
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?,
    );
    let value = write(&mut file)?;
    std::io::Write::flush(&mut file).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(value)
}

/// The per-page files, inside one stored zip. A zip entry carries its CRC
/// ahead of its data, so each page's file is buffered, one page at a time.
fn write_archive(job: &Job, outputs: &[PlannedOutput], tone: Tone, path: &Path) -> Result<(), String> {
    let file = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?,
    );
    let mut archive = Cbz::new(file);
    for output in outputs {
        let (_, bytes, patches) = page_inputs(job, output.page)?;
        let mut page = std::io::Cursor::new(Vec::new());
        export_page_to(&bytes, &patches, Target::SameAsSource, tone, &mut page)
            .map_err(|e| format!("{}: {e}", output.filename))?;
        archive.add(&output.filename, page.get_ref()).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    archive.finish().map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

/// One file for the whole strip, rows at a time with one page resident, and
/// its mask file beside it when asked for.
fn write_stitched(job: &Job, target: Target, tone: Tone, masks: MaskFiles, path: &Path, stage: &Path) -> Result<(), String> {
    let strip = strip_of(job);
    let global = global_patches(job, &strip)?;
    write_file(path, |file| {
        export_stitched(&strip, &global, &mut JobPages { job }, target, tone, file).map_err(|e| e.to_string())
    })?;
    if masks == MaskFiles::Beside {
        let strip_masks: Vec<&Mask> = global
            .iter()
            .map(StripPatch::in_strip_coordinates)
            .filter(|p| p.visible)
            .map(Patch::lettering)
            .collect();
        let mask_path = stage.join(format!("{}_mask.png", chapter_stem(job)));
        write_file(&mask_path, |file| {
            export_mask_to(&strip_masks, strip.width(), strip.height(), file)
                .map_err(|e| format!("{}: {e}", mask_path.display()))
        })?;
    }
    Ok(())
}

/// The job's pages, read off disk one at a time and held between no calls.
struct JobPages<'a> {
    job: &'a Job,
}

impl JobPages<'_> {
    fn bytes(&self, position: usize) -> Result<Vec<u8>, ExportError> {
        let sink = |message: String| ExportError::Sink(message);
        let source_idx = *self
            .job
            .project
            .strip
            .order
            .get(position)
            .ok_or_else(|| sink(format!("position {position} is not in the strip order")))?;
        let path = self
            .job
            .source_path(source_idx)
            .ok_or_else(|| sink(format!("source {source_idx} has no path")))?;
        std::fs::read(&path).map_err(|e| sink(format!("{}: {e}", path.display())))
    }
}

impl PageSource for JobPages<'_> {
    fn header(&mut self, position: usize) -> Result<Header, ExportError> {
        let bytes = self.bytes(position)?;
        let format = Format::sniff(&bytes)
            .ok_or(cleaner_core::image::ImageError::UnknownFormat)?;
        let mut header = match format {
            Format::Png => png_header(&bytes)?,
            Format::Tiff => tiff_header(&bytes)?,
            Format::Jpeg => cleaner_core::image::foreign::jpeg_header(&bytes)?,
        };
        (header.width, header.height) = cleaner_core::image::orientation::from_bytes(&bytes).size(header.width, header.height);
        Ok(header)
    }

    fn page(&mut self, position: usize) -> Result<Raster, ExportError> {
        let bytes = self.bytes(position)?;
        Ok(cleaner_core::image::orientation::from_bytes(&bytes).raster(&decode(&bytes)?))
    }
}

/// The whole patch set, lifted into strip coordinates. `export_stitched`
/// takes it as one slice, so every visible patch in the chapter is resident
/// while the strip is written; the page pixels are still one at a time.
fn global_patches(job: &Job, strip: &Strip) -> Result<Vec<StripPatch>, String> {
    let mut global = Vec::new();
    for position in 0..job.project.strip.order.len() {
        for patch in job.display_patches_on_page(position).map_err(|e| e.to_string())? {
            if !patch.visible { continue; }
            if let Some(lifted) = StripPatch::lift(strip, position, &patch) { global.push(lifted); }
        }
    }
    Ok(global)
}

/// The patches one page's file gets, in that page's own coordinates.
fn page_patches_for(job: &Job, position: usize) -> Result<Vec<Patch>, String> {
    job.native_patches_on_page(position).map_err(|e| e.to_string())
}

/// The strip's coordinate system, rebuilt from the manifest's sources and
/// order. Everything takes the strip position, never the source index.
fn strip_of(job: &Job) -> Strip {
    cleaner_core::project::orientation::strip(&job.project, true)
}

/// Every applied patch recorded against one source, reconstructed from the
/// manifest and its sidecar buffers.
///
/// **Ownership**, and correct for exactly the case it is used in: a `Single`
/// project, where a page is a page and there is no strip to span.
#[cfg(test)]
fn owned_patches(job: &Job, source_idx: usize) -> Result<Vec<Patch>, String> {
    job.project
        .patches
        .iter()
        .filter(|record| record.visible && record.source_idx == source_idx)
        .map(|record| load(job, record))
        .collect()
}

/// **The intersection**, for a longstrip job: every part of every patch in the
/// global set that lands on the page at `position`, in that page's own
/// coordinates.
///
/// The two steps are ordered by the memory rule, not by taste. Selection is
/// from the transformed bbox, computable from the manifest; only records that survive
/// it are loaded off disk. Building the whole global set first and then asking
/// which parts of it touch this page would hold every patch in the chapter at
/// once.
#[cfg(test)]
fn page_patches(job: &Job, strip: &Strip, position: usize) -> Result<Vec<Patch>, String> {
    let mut patches = Vec::new();
    for (anchor, record) in intersecting_records(job, strip, position) {
        if !record.visible {
            continue;
        }
        let patch = load(job, record)?;
        let Some(lifted) = StripPatch::lift(strip, anchor, &patch) else { continue };
        patches.extend(patches_on_page(strip, position, std::slice::from_ref(&lifted)));
    }
    Ok(patches)
}

/// Which patch records can reach the page at `position`, decided from
/// rectangles alone.
///
/// A record's displayed bbox is in the coordinate system of the page its `source_idx`
/// names, and a patch that spans a join is one whose bbox runs past that page's
/// own rectangle. That is the whole of the representation: there is no separate
/// "join-spanning" flag, and there does not need to be one, because the strip
/// geometry answers the question from the rectangle.
#[cfg(test)]
fn intersecting_records<'a>(
    job: &'a Job,
    strip: &Strip,
    position: usize,
) -> Vec<(usize, &'a PatchRecord)> {
    let Some(page) = strip.pages().get(position).map(|p| p.rect()) else { return Vec::new() };
    job.project
        .patches
        .iter()
        .filter_map(|record| {
            let anchor = job.project.strip.order.iter().position(|i| *i == record.source_idx)?;
            let displayed = record.display_bbox();
            let (x, y) = strip.to_strip(anchor, displayed.x, displayed.y)?;
            let bbox = Rect::new(x, y, displayed.w, displayed.h);
            (bbox.x < page.right()
                && bbox.right() > page.x
                && bbox.y < page.bottom()
                && bbox.bottom() > page.y)
                .then_some((anchor, record))
        })
        .collect()
}

#[cfg(test)]
fn load(job: &Job, record: &PatchRecord) -> Result<Patch, String> {
    job.load_patch(record).map_err(|e| format!("{}: {e}", record.id))
}

/// The directory the chapter's sources live in, for `'new-folder'`'s sibling
/// and for recognising `'source-folder'`. Taken from the first source: a
/// chapter's pages are ingested from one folder in practice, and
/// `Job::output_refusal` itself checks every source's own directory, so a
/// destination that matches only one of several source directories is still
/// caught correctly.
fn source_directory(job: &Job) -> PathBuf {
    // `origin_path` and not `source_path`: a page converted at ingest has its
    // file in the job's sidecar and its *origin* in the user's scan folder, and
    // both `'new-folder'` and `'source-folder'` are about the scan folder. A
    // sibling of the sidecar would put the export inside the application's own
    // library, where the user would never find it.
    job.origin_path(0)
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| job.root().to_path_buf())
}

/// What a one-file export is called, before its extension: the manifest's own
/// stem, which is the chapter's name on disk.
fn chapter_stem(job: &Job) -> String {
    job.path().file_stem().and_then(|s| s.to_str()).unwrap_or("chapter").to_owned()
}

fn output_filename(source: &ProjectSource, extension: &str) -> String {
    format!("{}.{extension}", source_stem(source))
}

/// `001_mask.png` beside `001.png`: the mask file is always a PNG, whatever
/// the image is, because it is an 8-bit grayscale mask and not a page.
fn mask_filename(source: &ProjectSource) -> String {
    format!("{}_mask.png", source_stem(source))
}

fn source_stem(source: &ProjectSource) -> &str {
    source.rel_path.file_stem().and_then(|s| s.to_str()).unwrap_or("page")
}

fn extension(format: Format) -> &'static str {
    match format {
        Format::Png => "png",
        Format::Tiff => "tiff",
        Format::Jpeg => "jpg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::composite::{changed_pixels, permitted_region};
    use cleaner_core::image::{Format, encode, fixtures};
    use cleaner_core::ingest::SourceRef;
    use cleaner_core::mask::Mask;
    use cleaner_core::patch::{Engine, Provenance};
    use cleaner_core::project::Project;
    use cleaner_core::text_shape::{MaskPlan, MaskQualityState, MASK_PLAN_VERSION};

    fn choices<'a>(format: &'a str, destination: &'a str, masks: &'a str, layout: &'a str) -> Choices<'a> {
        Choices { format, destination, masks, layout, pages: None, tone: Tone::Source, psd: Default::default() }
    }

    fn plan_open_job(job: &Job, format: &str, destination: &str, masks: &str, layout: &str) -> Result<ExportResult, String> {
        Ok(plan(job, &choices(format, destination, masks, layout))?.into_result())
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join("cleaner-tauri-export")
                .join(format!("{name}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
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

    fn write_source(dir: &Path, name: &str, fixture: &str) -> SourceRef {
        std::fs::create_dir_all(dir).unwrap();
        let bytes = encode(&fixtures::by_name(fixture).raster, Format::Png).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        cleaner_core::ingest::source_ref(&path, &bytes).unwrap()
    }

    /// A grayscale page narrower than the rest of the strip, which is the only
    /// thing that produces a gutter (the pages are centred).
    fn write_narrow_source(dir: &Path, name: &str, width: u32) -> SourceRef {
        std::fs::create_dir_all(dir).unwrap();
        let mut page = fixtures::by_name("l8").raster;
        page.data = (0..page.height)
            .flat_map(|y| (0..width).map(move |x| ((x + y) % 251) as u8))
            .collect();
        page.width = width;
        let bytes = encode(&page, Format::Png).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        cleaner_core::ingest::source_ref(&path, &bytes).unwrap()
    }

    fn a_job(scratch: &Scratch) -> Job {
        let source = write_source(&scratch.join("raws"), "001.png", "l8");
        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Single, &[source]);
        Job::create(&manifest, project).unwrap()
    }

    /// Three 64×48 grayscale pages, stacked. The joins are at strip rows 48 and
    /// 96.
    fn a_strip_job(scratch: &Scratch, mode: StripMode) -> Job {
        let sources: Vec<SourceRef> = (0..3)
            .map(|n| write_source(&scratch.join("raws"), &format!("{n:03}.png"), "l8"))
            .collect();
        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(manifest.parent().unwrap(), "0.1.0", mode, &sources);
        Job::create(&manifest, project).unwrap()
    }

    /// A rung-0 planar fill anchored on `source_idx`, in that page's own
    /// coordinates - and deliberately taller than the page, which is what a
    /// patch that spans a join looks like in a manifest. `PatchRecord.bbox` has
    /// no separate flag for it and needs none: the strip geometry answers from
    /// the rectangle.
    fn overhanging_fill(job: &mut Job, source_idx: usize, bounds: Rect, tone: u8) {
        let page = fixtures::by_name("l8").raster;
        let mut pixels = page.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.icc = None;
        pixels.data = vec![tone; (bounds.w * bounds.h) as usize];
        let patch = Patch {
            id: "across-the-join".into(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 0,
            visible: true,
            provenance: provenance(),
        };
        job.complete_region(source_idx, &patch, None).unwrap();
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

    #[test]
    fn tampered_text_shape_sidecar_refuses_export_before_any_output() {
        let scratch = Scratch::new("tampered-support-export");
        let mut job = a_job(&scratch);
        let source = &job.project.sources[0];
        let mut base = Mask::empty(Rect::new(10, 10, 3, 2));
        base.set(10, 10, true);
        base.set(12, 11, true);
        let empty = Mask::empty(Rect::new(0, 0, 0, 0));
        let plan = MaskPlan {
            version: MASK_PLAN_VERSION,
            region_id: "shape".into(),
            source_sha256: source.sha256.clone(),
            lower_composite_sha256: "test-underlay".into(),
            algorithm_id: "synthetic-test".into(),
            model_id: None,
            candidate_bounds: Rect::new(8, 8, 8, 8),
            refinement_crop: Rect::new(8, 8, 8, 8),
            base_revision: 1,
            correction_revision: 1,
            plan_revision: 1,
            base_mask: base.into(),
            additions: empty.clone().into(),
            removals: empty.into(),
            padding_px: 0,
            model_hole_margin_px: 0,
            reading_context: Rect::new(0, 0, source.w, source.h),
            blend_alpha: None,
            quality: MaskQualityState::Ready,
        };
        let prepared = plan.prepare(source.w, source.h).unwrap();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();
        let support = prepared.write_support.to_mask();
        let patch = Patch {
            id: "shape".into(), mask: support.clone(), ink: support,
            pixels: cleaner_core::engines::model::page_crop(&fixtures::by_name("l8").raster,
                prepared.write_support.bounds),
            order: 0, visible: true, provenance: provenance(),
        };
        job.complete_text_shape_region(0, &patch, &prepared.identity.identity_sha256, None).unwrap();
        let row = &job.project.patches[0];
        let mut tampered = patch.mask;
        tampered.set(11, 10, true);
        std::fs::write(job.sidecar().join(&row.mask_ref),
            cleaner_core::project::buffers::encode_mask(&tampered)).unwrap();
        assert!(matches!(
            crate::tile::render(job.path(), 0, crate::tile::Variant::Cleaned, Some(0)),
            Err(crate::tile::TileError::Store(cleaner_core::project::StoreError::StalePlan))
        ));
        let destination = scratch.join("output");
        let result = export_open_job(&job, "PNG", destination.to_str().unwrap(), "flattened", "per-page")
            .unwrap();
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.invalidSource"));
        assert!(result.detail.unwrap().contains("prepared text-shaped mask plan"));
        assert!(!destination.exists(), "export created output before validating patches");
    }

    #[test]
    fn text_shaped_mask_file_uses_the_previewed_write_support() {
        let mut support = Mask::empty(Rect::new(10, 10, 5, 5));
        support.set(10, 10, true);
        support.set(14, 14, true);
        let mut ink = Mask::empty(support.bounds);
        ink.set(10, 10, true);
        let mut provenance = provenance();
        provenance.params_snapshot = serde_json::json!({"geometry_policy": "text_shape"});
        let patch = Patch {
            id: "text-shape".into(), mask: support, ink,
            pixels: cleaner_core::engines::model::page_crop(&fixtures::by_name("l8").raster,
                Rect::new(10, 10, 5, 5)),
            order: 0, visible: true, provenance,
        };
        let path = std::env::temp_dir().join(format!("mc-text-shape-mask-{}.png", std::process::id()));
        write_mask_file(&path, &[patch], 64, 48).unwrap();
        let exported = cleaner_core::image::decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(exported.sample(10, 10, 0), 255);
        assert_eq!(exported.sample(14, 14, 0), 255, "padding belongs to exported W");
        assert_eq!(exported.sample(12, 12, 0), 0, "the gap remains outside W");
        std::fs::remove_file(path).ok();
    }

    /// `export_open_job` with the two parameters most tests do not vary.
    fn export(job: &Job, format: &str, destination: &str) -> ExportResult {
        export_open_job(job, format, destination, "flattened", "per-page").unwrap()
    }

    /// A page with no applied patches is copied byte-for-byte, and lands in
    /// the sibling `_cleaned` directory `default_output_dir` names.
    #[test]
    fn planning_is_read_only_and_revision_covers_native_source_bytes() {
        let scratch = Scratch::new("color-plan");
        let job = a_strip_job(&scratch, StripMode::Single);
        let out = scratch.join("planned");
        let plan = plan_open_job(&job, "PNG", &out.to_string_lossy(), "flattened", "per-page").unwrap();
        assert_eq!(plan.status, "planned");
        assert_eq!(plan.actual_formats, ["PNG"]);
        assert!(!out.exists(), "planning wrote output files");
        let before = plan.revision;
        let path = job.source_path(0).unwrap();
        let mut bytes = std::fs::read(&path).unwrap(); bytes.push(0);
        std::fs::write(path, bytes).unwrap();
        assert_ne!(before.unwrap(), revision(&job, &choices("PNG", &out.to_string_lossy(), "flattened", "per-page")).unwrap());
    }

    #[test]
    fn reviewed_revision_refuses_changed_patch_before_output_is_created() {
        let scratch = Scratch::new("stale-plan");
        let mut job = a_job(&scratch);
        let out = scratch.join("export");
        let plan = plan_open_job(&job, "PNG", out.to_str().unwrap(), "flattened", "per-page").unwrap();
        fill_page(&mut job, 0, Rect::new(1, 1, 2, 2), 42);
        let result = export_reviewed_job(&job, &choices("PNG", out.to_str().unwrap(), "flattened", "per-page"), plan.revision.as_deref()).unwrap();
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.stalePlan"));
        assert!(!out.exists());
    }

    #[test]
    fn a_page_selection_exports_those_pages_alone() {
        let scratch = Scratch::new("one-page");
        let job = a_strip_job(&scratch, StripMode::Single);
        let out = scratch.join("export");
        for format in ["PNG", "PSD"] {
            let dir = out.join(format);
            let choices = Choices { pages: Some(&[1]), ..choices(format, dir.to_str().unwrap(), "flattened", "per-page") };
            let result = export_reviewed_job(&job, &choices, None).unwrap();
            assert_eq!(result.status, "exported", "{format}: {:?}", result.detail);
            assert_eq!(result.file_count, Some(1));
            assert_eq!(result.outputs.iter().map(|o| o.page).collect::<Vec<_>>(), [1]);
            let written: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert_eq!(written.len(), 1, "{format} wrote {written:?}");
        }
    }

    /// Three grey pages stored as RGB, in a longstrip so every container is
    /// open to them.
    fn a_grey_rgb_strip_job(scratch: &Scratch) -> Job {
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let sources: Vec<SourceRef> = (0..3)
            .map(|n| {
                let mut page = fixtures::by_name("rgb8").raster;
                for y in 0..page.height {
                    for x in 0..page.width {
                        let v = page.sample(x, y, 0);
                        page.set_sample(x, y, 1, v);
                        page.set_sample(x, y, 2, v);
                    }
                }
                let bytes = encode(&page, Format::Png).unwrap();
                let path = raws.join(format!("{n:03}.png"));
                std::fs::write(&path, &bytes).unwrap();
                cleaner_core::ingest::source_ref(&path, &bytes).unwrap()
            })
            .collect();
        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Longstrip, &sources);
        Job::create(&manifest, project).unwrap()
    }

    #[test]
    fn grayscale_writes_grey_rgb_pages_gray_in_every_container() {
        use cleaner_core::image::ColorMode;
        let scratch = Scratch::new("grayscale");
        let mut job = a_grey_rgb_strip_job(&scratch);
        let bounds = Rect::new(10, 20, 20, 16);
        let mut pixels = fixtures::by_name("rgb8").raster;
        (pixels.width, pixels.height) = (bounds.w, bounds.h);
        pixels.data = vec![200; (bounds.w * bounds.h * 3) as usize];
        let patch = Patch {
            id: "grey-fill".into(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 0,
            visible: true,
            provenance: provenance(),
        };
        job.complete_region(0, &patch, None).unwrap();
        let out = scratch.join("export");
        let mode = |bytes: &[u8]| cleaner_core::image::decode(bytes).unwrap().mode;
        for (tone, expected, psd_mode) in [(Tone::Source, ColorMode::Rgb, 3), (Tone::Grayscale, ColorMode::Gray, 1)] {
            let dir = out.join(format!("{tone:?}"));
            for (format, layout) in [("PNG", "per-page"), ("TIFF", "per-page"), ("CBZ", "per-page"), ("PNG", "stitched"), ("PSD", "per-page")] {
                let dest = dir.join(format!("{format}-{layout}"));
                let choices = Choices { tone, ..choices(format, dest.to_str().unwrap(), "flattened", layout) };
                let result = export_reviewed_job(&job, &choices, None).unwrap();
                assert_eq!(result.status, "exported", "{tone:?} {format} {layout}: {:?}", result.detail);
                let files: Vec<PathBuf> = std::fs::read_dir(&dest).unwrap().map(|e| e.unwrap().path()).collect();
                for file in files {
                    let bytes = std::fs::read(&file).unwrap();
                    match format {
                        "CBZ" => for (name, entry) in unzip(&bytes) {
                            assert_eq!(mode(&entry), expected, "{tone:?} {name} in the archive");
                        },
                        "PSD" => assert_eq!(psd_header(&bytes).4, psd_mode, "{tone:?} {}", file.display()),
                        _ => assert_eq!(mode(&bytes), expected, "{tone:?} {}", file.display()),
                    }
                }
            }
        }
        let source = choices("PNG", out.to_str().unwrap(), "flattened", "per-page");
        let gray = Choices { tone: Tone::Grayscale, ..source };
        assert_ne!(revision(&job, &source).unwrap(), revision(&job, &gray).unwrap(), "a plan for one tone is stale for the other");
    }

    #[test]
    fn a_page_selection_outside_the_chapter_or_of_a_strip_is_refused() {
        let scratch = Scratch::new("bad-pages");
        let job = a_strip_job(&scratch, StripMode::Longstrip);
        let out = scratch.join("export");
        let dest = out.to_str().unwrap();
        for (layout, pages) in [("per-page", &[3][..]), ("per-page", &[][..]), ("stitched", &[0][..])] {
            let choices = Choices { pages: Some(pages), ..choices("PNG", dest, "flattened", layout) };
            let result = plan(&job, &choices).unwrap().into_result();
            assert_eq!(result.reason_key, Some("notice.export.refusedPageSelection"), "{layout} {pages:?}");
        }
        let cbz = Choices { pages: Some(&[1]), ..choices("CBZ", dest, "flattened", "per-page") };
        assert_eq!(plan(&job, &cbz).unwrap().into_result().reason_key, Some("notice.export.refusedPageSelection"));
        let every = Choices { pages: Some(&[2, 0, 1, 1]), ..choices("PNG", dest, "flattened", "stitched") };
        assert_eq!(plan(&job, &every).unwrap().into_result().status, "planned");
        assert!(!out.exists());
    }

    #[test]
    fn rotated_native_edit_and_ink_match_per_page_cbz_and_stitched_outputs() {
        let scratch = Scratch::new("oriented-writers");
        let raws = scratch.join("raws"); std::fs::create_dir_all(&raws).unwrap();
        let refs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/cleaner-core/tests/fixtures/color-reference");
        let sources: Vec<_> = [6, 1].iter().enumerate().map(|(index, orientation)| {
            let bytes = std::fs::read(refs.join(format!("orientation-{orientation}.jpg"))).unwrap();
            let path = raws.join(format!("{index:03}.jpg")); std::fs::write(&path, &bytes).unwrap();
            cleaner_core::ingest::source_ref(&path, &bytes).unwrap()
        }).collect();
        let manifest = scratch.join("chapter.mtclean");
        let mut job = Job::create(&manifest, Project::new(&scratch.0, "test", StripMode::Longstrip, &sources)).unwrap();
        let native = source_raster(&job, 0);
        let mut pixels = native.clone(); pixels.width = 1; pixels.height = 1; pixels.data = vec![12, 34, 56];
        job.complete_region(0, &Patch { id: "native-corner".into(), mask: Mask::filled(Rect::new(0, 0, 1, 1)), ink: Mask::filled(Rect::new(0, 0, 1, 1)), pixels, order: 0, visible: true, provenance: provenance() }, None).unwrap();
        let (x, y) = cleaner_core::image::orientation::Orientation(6).pixel(0, 0, native.width, native.height);
        let result = export_open_job(&job, "PNG", scratch.join("png").to_str().unwrap(), "separate-layer", "per-page").unwrap();
        let dir = PathBuf::from(result.path.unwrap());
        let encoded = std::fs::read(dir.join("000.png")).unwrap();
        assert_eq!(cleaner_core::image::orientation::from_bytes(&encoded).0, 1, "re-encoded samples must not carry stale EXIF rotation");
        let mut at = 8;
        while at + 12 <= encoded.len() {
            assert_ne!(&encoded[at + 4..at + 8], b"eXIf", "stale EXIF must be omitted after orienting samples");
            at += 12 + u32::from_be_bytes(encoded[at..at + 4].try_into().unwrap()) as usize;
        }
        let page = decode(&encoded).unwrap();
        let mask = decode(&std::fs::read(dir.join("000_mask.png")).unwrap()).unwrap();
        assert_eq!((page.width, page.height), (native.height, native.width));
        assert_eq!((mask.width, mask.height), (page.width, page.height));
        assert_eq!(mask.sample(x as u32, y as u32, 0), 255);
        assert_eq!(page.sample(x as u32, y as u32, 0), 12);
        let result = export(&job, "CBZ", scratch.join("cbz").to_str().unwrap());
        let entries = unzip(&std::fs::read(result.path.unwrap()).unwrap());
        assert_eq!(decode(&entries[0].1).unwrap().data, page.data);
        let result = export_open_job(&job, "PNG", scratch.join("stitched").to_str().unwrap(), "flattened", "stitched").unwrap();
        let stitched = decode(&std::fs::read(result.path.unwrap()).unwrap()).unwrap();
        let strip = strip_of(&job); let first = strip.pages()[0];
        assert_eq!(stitched.sample((x + first.x_offset) as u32, y as u32, 0), 12);
        assert_eq!(stitched.height, native.width + job.project.sources[1].h);
    }

    #[test]
    fn mixed_cbz_reports_actual_formats_and_keeps_untouched_jpeg_bytes() {
        use image::ImageEncoder;
        let scratch = Scratch::new("mixed-cbz");
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let page = fixtures::by_name("rgb8").raster;
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).write_image(&page.data, page.width, page.height, image::ExtendedColorType::Rgb8).unwrap();
        let sources: Vec<_> = ["001.jpg", "002.jpeg"].iter().map(|name| {
            let path = raws.join(name); std::fs::write(&path, &jpeg).unwrap();
            cleaner_core::ingest::source_ref(&path, &jpeg).unwrap()
        }).collect();
        let manifest = scratch.join("chapter.mtclean");
        let mut job = Job::create(&manifest, Project::new(&scratch.0, "test", StripMode::Single, &sources)).unwrap();
        let mut pixels = page.clone(); pixels.width = 1; pixels.height = 1; pixels.data = vec![12, 34, 56];
        let patch = Patch { id: "edit-jpeg".into(), mask: Mask::filled(Rect::new(1, 1, 1, 1)), ink: Mask::filled(Rect::new(1, 1, 1, 1)), pixels, order: 0, visible: true, provenance: provenance() };
        job.complete_region(1, &patch, None).unwrap();
        let out = scratch.join("export");
        let plan = plan_open_job(&job, "CBZ", out.to_str().unwrap(), "flattened", "per-page").unwrap();
        assert_eq!(plan.actual_formats, ["JPEG", "PNG"]);
        assert_eq!(plan.outputs.iter().map(|p| p.filename.as_str()).collect::<Vec<_>>(), ["001.jpg", "002.png"]);
        assert!(plan.declared.iter().any(|d| d.page == 1 && d.key == "export.declared.formatChanged"));
        let result = export(&job, "CBZ", out.to_str().unwrap());
        assert_eq!(result.actual_formats, plan.actual_formats);
        let archive = unzip(&std::fs::read(result.path.unwrap()).unwrap());
        assert_eq!(archive[0], ("001.jpg".into(), jpeg));
        assert_eq!(Format::sniff(&archive[1].1), Some(Format::Png));
    }

    #[test]
    fn runtime_publish_failure_reports_completed_files_and_cleans_stage() {
        let scratch = Scratch::new("partial-publish");
        let job = a_strip_job(&scratch, StripMode::Single);
        let out = scratch.join("export");
        // The second target is a directory: encoding succeeds, publication fails.
        std::fs::create_dir_all(out.join("001.png")).unwrap();
        let result = export(&job, "PNG", out.to_str().unwrap());
        assert_eq!(result.status, "partial");
        assert_eq!(result.file_count, Some(1));
        assert!(result.detail.as_ref().unwrap().contains("001.png"));
        assert!(result.detail.as_ref().unwrap().contains("000.png"));
        assert!(out.join("000.png").is_file());
        assert!(!out.join("002.png").exists());
        assert!(!std::fs::read_dir(&out).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with(".export-")));
        assert_eq!(announcement(&result, "PNG").unwrap().0, "notice.export.partial");
    }

    #[test]
    fn colliding_output_names_refuse_before_publishing_either_page() {
        let scratch = Scratch::new("collision-plan");
        let mut job = a_strip_job(&scratch, StripMode::Single);
        let old = job.source_path(1).unwrap();
        job.project.sources[1].rel_path.set_file_name("000.jpg");
        std::fs::copy(old, job.source_path(1).unwrap()).unwrap();
        let out = scratch.join("export");
        let result = export(&job, "PNG", out.to_str().unwrap());
        assert_eq!(result.status, "refused");
        assert!(result.detail.unwrap().contains("000.png"));
        assert!(!out.exists());
    }

    #[test]
    fn invalid_later_page_publishes_nothing() {
        let scratch = Scratch::new("color-invalid");
        let job = a_strip_job(&scratch, StripMode::Single);
        std::fs::write(job.source_path(1).unwrap(), b"not an image").unwrap();
        let out = scratch.join("planned");
        let result = export_open_job(&job, "TIFF", &out.to_string_lossy(), "flattened", "per-page").unwrap();
        assert_eq!(result.status, "refused");
        assert!(!out.exists());
        assert!(result.detail.unwrap().contains("page 2"));
    }

    #[test]
    fn a_page_with_no_patches_is_exported_byte_for_byte() {
        let scratch = Scratch::new("passthrough");
        let job = a_job(&scratch);
        let source_bytes = std::fs::read(job.source_path(0).unwrap()).unwrap();

        let result = export(&job, "PNG", "new-folder");
        assert_eq!(result.status, "exported");
        assert_eq!(result.file_count, Some(1));

        let out_dir = PathBuf::from(result.path.unwrap());
        assert_eq!(out_dir, scratch.join("raws_cleaned").canonicalize().unwrap());
        assert_eq!(std::fs::read(out_dir.join("001.png")).unwrap(), source_bytes);
    }

    /// The refusal this module must not re-decide: `Job::output_refusal`
    /// already says no, and this pins that the command reports it exactly:
    /// `status: 'refused'`, the one reasonKey, no file count,
    /// and the directory that was refused.
    #[test]
    fn exporting_to_the_source_folder_is_refused_rather_than_written() {
        let scratch = Scratch::new("refuse");
        let job = a_job(&scratch);

        let result = export(&job, "PNG", "source-folder");
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.refusedOverwrite"));
        assert_eq!(result.file_count, None);
        assert!(
            result.path.is_some_and(|path| path.ends_with("raws")),
            "the refusal did not name the directory it refused"
        );
        assert!(std::fs::read_dir(scratch.join("raws_cleaned")).is_err(), "wrote anyway");
    }

    /// An applied patch reconstructed from the manifest and its sidecar
    /// buffers reaches `export_page`, and the composited page - not the raw
    /// source - is what lands on disk.
    #[test]
    fn a_completed_region_is_composited_into_the_export() {
        let scratch = Scratch::new("patch");
        let mut job = a_job(&scratch);
        let source_bytes = std::fs::read(job.source_path(0).unwrap()).unwrap();
        fill_page(&mut job, 0, Rect::new(4, 4, 8, 8), 10);

        let result = export(&job, "PNG", "new-folder");
        assert_eq!(result.status, "exported");
        assert_eq!(result.file_count, Some(1));

        let out_dir = PathBuf::from(result.path.unwrap());
        let written = std::fs::read(out_dir.join("001.png")).unwrap();
        assert_ne!(written, source_bytes, "the patch changed nothing");
    }

    /// The flattened export at 0, 50 and 100 percent layer opacity: the page,
    /// the rounded half-way blend, and the patch - the same pixels the tile
    /// test in `tile.rs` pins for the screen.
    #[test]
    fn zero_half_and_full_opacity_export_the_expected_pixels() {
        for opacity in [0u8, 50, 100] {
            let scratch = Scratch::new(&format!("export-opacity-{opacity}"));
            let mut job = a_job(&scratch);
            fill_page(&mut job, 0, Rect::new(4, 4, 8, 8), 10);
            job.project.patches[0].provenance.params_snapshot["layer"] = serde_json::json!({"opacity": opacity});
            job.flush().unwrap();

            let result = export(&job, "PNG", "new-folder");
            assert_eq!(result.status, "exported");
            let written = cleaner_core::image::decode(
                &std::fs::read(PathBuf::from(result.path.unwrap()).join("001.png")).unwrap(),
            ).unwrap();
            let source = source_raster(&job, 0);
            for (x, y) in [(4u32, 4u32), (8, 9), (11, 11)] {
                let below = source.sample(x, y, 0);
                let wanted = match opacity {
                    0 => below,
                    100 => 10,
                    _ => ((below as f64 + 10.0) / 2.0).round() as u16,
                };
                assert_eq!(written.sample(x, y, 0), wanted, "{opacity}% at ({x}, {y})");
            }
            assert_eq!(written.sample(30, 30, 0), source.sample(30, 30, 0));
        }
    }

    /// A patch wholly inside one page, for the tests that only need the export
    /// to have something to composite.
    fn fill_page(job: &mut Job, source_idx: usize, bounds: Rect, tone: u8) {
        let mut pixels = fixtures::by_name("l8").raster;
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.icc = None;
        pixels.data = vec![tone; (bounds.w * bounds.h) as usize];
        let patch = Patch {
            id: format!("p{source_idx}"),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 0,
            visible: true,
            provenance: provenance(),
        };
        job.complete_region(source_idx, &patch, None).unwrap();
    }

    /* -------------------------------------------------------------- */
    /* The refusals                                                    */
    /* -------------------------------------------------------------- */

    /// **The defect this phase came for.** `target_for` used to map every
    /// format it did not recognise to `Target::SameAsSource`, so a user who
    /// chose JPEG got PNG and was told the export succeeded. Half of the old
    /// argument survives - the fallback never produced *lossy* output, so
    /// that contract was never violated by it - and the other half does not:
    /// it produced a container nobody asked for, without saying so.
    #[test]
    fn a_format_this_build_cannot_write_is_refused_by_name() {
        assert_eq!(requested_format("PNG"), Ok(Requested::Png));
        assert_eq!(requested_format("png"), Ok(Requested::Png));
        assert_eq!(requested_format("TIFF"), Ok(Requested::Tiff));
        assert_eq!(requested_format("tif"), Ok(Requested::Tiff));
        assert_eq!(requested_format("CBZ"), Ok(Requested::Cbz));

        assert_eq!(requested_format("JPEG"), Err("notice.export.refusedLossyFormat"));
        assert_eq!(requested_format("jpg"), Err("notice.export.refusedLossyFormat"));
        assert_eq!(requested_format("WebP"), Err("notice.export.refusedLossyFormat"));
        assert_eq!(requested_format("PSD"), Ok(Requested::Psd));
        assert_eq!(requested_format("psb"), Err("notice.export.refusedLayeredFormat"));
        assert_eq!(requested_format("AVIF"), Err("notice.export.refusedUnknownFormat"));
        assert_eq!(requested_format(""), Err("notice.export.refusedUnknownFormat"));
    }

    /// And the refusal reaches disk as a refusal: nothing is created, and the
    /// result carries the reason rather than a file count.
    #[test]
    fn a_refused_format_writes_nothing_at_all() {
        let scratch = Scratch::new("jpeg");
        let job = a_job(&scratch);

        let result = export(&job, "JPEG", "new-folder");
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.refusedLossyFormat"));
        assert_eq!(result.file_count, None);
        assert!(
            std::fs::read_dir(scratch.join("raws_cleaned")).is_err(),
            "a refused format created the output directory"
        );
    }

    /// `masks` branches. For a raster format, `'separate-layer'` is a mask file
    /// beside each page - `001.png` and `001_mask.png` - white where the
    /// lettering was and black everywhere else, the page's own size. The
    /// fixture's ink is its whole mask, so the two readings agree here; the
    /// test after this one is where they differ.
    #[test]
    fn separate_layer_writes_a_mask_file_beside_each_page() {
        let scratch = Scratch::new("mask-files");
        let mut job = a_job(&scratch);
        fill_page(&mut job, 0, Rect::new(4, 4, 8, 8), 10);

        let result =
            export_open_job(&job, "PNG", "new-folder", "separate-layer", "per-page").unwrap();
        assert_eq!(result.status, "exported");
        assert_eq!(result.file_count, Some(1), "the count is pages, not files");
        let out_dir = PathBuf::from(result.path.unwrap());
        assert!(out_dir.join("001.png").exists());

        let mask = cleaner_core::image::decode(&std::fs::read(out_dir.join("001_mask.png")).unwrap())
            .unwrap();
        assert_eq!((mask.width, mask.height), (64, 48));
        assert_eq!(mask.mode, cleaner_core::image::ColorMode::Gray);
        assert_eq!(mask.depth, cleaner_core::image::BitDepth::Eight);
        for y in 0..48 {
            for x in 0..64 {
                let inside = (4..12).contains(&x) && (4..12).contains(&y);
                assert_eq!(mask.sample(x, y, 0), if inside { 255 } else { 0 }, "({x}, {y})");
            }
        }

        // TIFF pages get the same PNG mask beside them: a mask is not a page.
        let tiff = export_open_job(
            &job,
            "TIFF",
            scratch.join("tiff").to_str().unwrap(),
            "separate-layer",
            "per-page",
        )
        .unwrap();
        let tiff_dir = PathBuf::from(tiff.path.unwrap());
        assert!(tiff_dir.join("001.tiff").exists());
        assert!(tiff_dir.join("001_mask.png").exists());

        // A flattened export writes no mask file, which is what it always did.
        let flat = export(&job, "PNG", scratch.join("flat").to_str().unwrap());
        assert!(!PathBuf::from(flat.path.unwrap()).join("001_mask.png").exists());

        assert_eq!(mask_files("flattened"), MaskFiles::None);
        assert_eq!(mask_files("separate-layer"), MaskFiles::Beside);
    }

    /// **The mask file is the lettering, not the fill.** A balloon rung 0
    /// cleaned has an applied mask the size of the balloon, and a mask file
    /// made of applied masks was a page of white blobs where the balloons were.
    /// The file is made of each patch's ink instead: white where the text was,
    /// black over the rest of the fill.
    #[test]
    fn the_mask_file_is_the_ink_and_not_the_applied_mask() {
        let scratch = Scratch::new("mask-files-ink");
        let mut job = a_job(&scratch);
        let balloon = Rect::new(4, 4, 24, 20);
        let lettering = Rect::new(10, 8, 6, 10);
        let mut pixels = fixtures::by_name("l8").raster;
        pixels.width = balloon.w;
        pixels.height = balloon.h;
        pixels.icc = None;
        pixels.data = vec![10; (balloon.w * balloon.h) as usize];
        let patch = Patch {
            id: "balloon".into(),
            mask: Mask::filled(balloon),
            ink: Mask::filled(lettering),
            pixels,
            order: 0,
            visible: true,
            provenance: provenance(),
        };
        job.complete_region(0, &patch, None).unwrap();

        let result =
            export_open_job(&job, "PNG", "new-folder", "separate-layer", "per-page").unwrap();
        let out_dir = PathBuf::from(result.path.unwrap());
        let mask = cleaner_core::image::decode(&std::fs::read(out_dir.join("001_mask.png")).unwrap())
            .unwrap();
        for y in 0..48 {
            for x in 0..64 {
                let text = lettering.contains(i64::from(x), i64::from(y));
                assert_eq!(mask.sample(x, y, 0), if text { 255 } else { 0 }, "({x}, {y})");
            }
        }

        // The page itself is still the whole fill: the ink changes what the
        // mask file says, not what was painted.
        let page = cleaner_core::image::decode(&std::fs::read(out_dir.join("001.png")).unwrap())
            .unwrap();
        assert_eq!(page.sample(5, 5, 0), 10, "the fill reaches the balloon's edge");
        assert_eq!(mask_files("sideways"), MaskFiles::None, "an unknown choice is the default");
    }

    /// A mask file inside a CBZ would be read as a page, so the pair is
    /// refused rather than packed.
    #[test]
    fn a_mask_file_cannot_go_inside_an_archive() {
        let scratch = Scratch::new("mask-cbz");
        let job = a_job(&scratch);

        let result =
            export_open_job(&job, "CBZ", "new-folder", "separate-layer", "per-page").unwrap();
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.refusedMaskLayers"));
        assert!(std::fs::read_dir(scratch.join("raws_cleaned")).is_err(), "wrote anyway");
    }

    /* -------------------------------------------------------------- */
    /* PSD                                                             */
    /* -------------------------------------------------------------- */

    /// The header fields of a PSD, read the way a reader would: enough to
    /// know the file is the page's size, mode and depth. The layer structure
    /// is pinned in `cleaner_core::export::psd`'s own tests against a
    /// specification reader; here the claim is that the command reaches it.
    fn psd_header(bytes: &[u8]) -> (u16, u32, u32, u16, u16) {
        assert_eq!(&bytes[..4], b"8BPS");
        let be16 = |at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]);
        let be32 = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        (be16(12), be32(14), be32(18), be16(22), be16(24))
    }

    /// The UTF-16BE `luni` block Photoshop reads a layer's name from.
    fn has_layer_named(bytes: &[u8], name: &str) -> bool {
        let mut needle = b"8BIMluni".to_vec();
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut data = (units.len() as u32).to_be_bytes().to_vec();
        for unit in units {
            data.extend_from_slice(&unit.to_be_bytes());
        }
        if data.len() % 2 == 1 {
            data.push(0);
        }
        needle.extend_from_slice(&(data.len() as u32).to_be_bytes());
        needle.extend_from_slice(&data);
        bytes.windows(needle.len()).any(|w| w == needle)
    }

    /// The layered shape: one `.psd` per page, the untouched source as
    /// `Background`, each region a masked layer in a `Cleaned` group - and,
    /// flattened, one `Background` layer of the composite with no group.
    #[test]
    fn a_psd_is_written_layered_or_flattened_as_asked() {
        let scratch = Scratch::new("psd");
        let mut job = a_strip_job(&scratch, StripMode::Longstrip);
        overhanging_fill(&mut job, 0, Rect::new(10, 40, 20, 16), 200);

        let layered = export_open_job(
            &job,
            "PSD",
            scratch.join("layered").to_str().unwrap(),
            "separate-layer",
            "per-page",
        )
        .unwrap();
        assert_eq!(layered.status, "exported");
        assert_eq!(layered.file_count, Some(3));
        let layered_dir = PathBuf::from(layered.path.unwrap());
        for n in 0..3 {
            let bytes = std::fs::read(layered_dir.join(format!("{n:03}.psd"))).unwrap();
            assert_eq!(psd_header(&bytes), (1, 48, 64, 8, 1), "page {n}: channels, h, w, depth, mode");
            assert!(has_layer_named(&bytes, "Background"), "page {n}");
        }
        // The patch spans the join, so pages 0 and 1 have a region and page 2
        // has none - and no group.
        let page0 = std::fs::read(layered_dir.join("000.psd")).unwrap();
        let page1 = std::fs::read(layered_dir.join("001.psd")).unwrap();
        let page2 = std::fs::read(layered_dir.join("002.psd")).unwrap();
        assert!(has_layer_named(&page0, "Cleaned") && has_layer_named(&page0, "region-0001"));
        assert!(has_layer_named(&page1, "Cleaned") && has_layer_named(&page1, "region-0001"));
        assert!(!has_layer_named(&page2, "Cleaned"));

        let flat = export_open_job(
            &job,
            "PSD",
            scratch.join("flat").to_str().unwrap(),
            "flattened",
            "per-page",
        )
        .unwrap();
        assert_eq!(flat.file_count, Some(3));
        let flat_page0 = std::fs::read(PathBuf::from(flat.path.unwrap()).join("000.psd")).unwrap();
        assert!(has_layer_named(&flat_page0, "Background"));
        assert!(!has_layer_named(&flat_page0, "Cleaned"), "a flattened PSD has no group");
        assert!(flat_page0.len() < page0.len(), "one layer is smaller than three");
    }

    /// A page PSD cannot carry refuses the chapter from the manifest, before
    /// a directory exists; and a stitched PSD is refused by name.
    #[test]
    fn a_psd_of_what_psd_cannot_carry_is_refused_before_anything_is_written() {
        let scratch = Scratch::new("psd-refused");
        let raws = scratch.join("raws");
        let sources = vec![
            write_source(&raws, "000.png", "l8"),
            write_source(&raws, "001.png", "indexed-p"),
        ];
        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project =
            Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Single, &sources);
        let job = Job::create(&manifest, project).unwrap();

        let result = export_open_job(&job, "PSD", "new-folder", "flattened", "per-page").unwrap();
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.refusedLayeredMode"));
        assert!(std::fs::read_dir(scratch.join("raws_cleaned")).is_err(), "wrote anyway");

        let strip_job = a_strip_job(&scratch, StripMode::Longstrip);
        let stitched =
            export_open_job(&strip_job, "PSD", "new-folder", "flattened", "stitched").unwrap();
        assert_eq!(stitched.status, "refused");
        assert_eq!(stitched.reason_key, Some("notice.export.refusedStitchedLayered"));
    }

    /// Settings > Export reaches the file: raw channels make it bigger, and
    /// the Lettering layer is in it. A plan made under other PSD settings is
    /// stale.
    #[test]
    fn psd_settings_reach_the_file_and_the_plan_revision() {
        let scratch = Scratch::new("psd-settings");
        let mut job = a_strip_job(&scratch, StripMode::Single);
        overhanging_fill(&mut job, 1, Rect::new(10, 10, 8, 8), 200);
        let chosen = psd::PsdOptions { compression: psd::Compression::Raw, lettering_layer: true };
        let mut files = Vec::new();
        let mut revisions = Vec::new();
        for (name, options) in [("default", psd::PsdOptions::default()), ("chosen", chosen)] {
            let dir = scratch.join(name);
            let choices = Choices { psd: options, ..choices("PSD", dir.to_str().unwrap(), "separate-layer", "per-page") };
            revisions.push(plan(&job, &choices).unwrap().into_result().revision.unwrap());
            let result = export_reviewed_job(&job, &choices, None).unwrap();
            assert_eq!(result.status, "exported", "{name}: {:?}", result.detail);
            let output = result.outputs.iter().find(|o| o.page == 1).unwrap();
            files.push(std::fs::read(dir.join(&output.filename)).unwrap());
        }
        let has_lettering = |bytes: &[u8]| bytes.windows(9).any(|w| w == psd::LETTERING_LAYER.as_bytes());
        assert!(!has_lettering(&files[0]));
        assert!(has_lettering(&files[1]));
        assert!(files[1].len() > files[0].len(), "raw channels are bigger");
        assert_ne!(revisions[0], revisions[1]);

        let dir = scratch.join("stale");
        let first = Choices { psd: chosen, ..choices("PSD", dir.to_str().unwrap(), "separate-layer", "per-page") };
        let planned = plan(&job, &first).unwrap().into_result().revision.unwrap();
        let now = Choices { psd: psd::PsdOptions::default(), ..first };
        let result = export_reviewed_job(&job, &now, Some(&planned)).unwrap();
        assert_eq!(result.reason_key, Some("notice.export.stalePlan"));
        assert!(!dir.exists());
    }

    #[test]
    fn oversized_psd_page_refuses_chapter_before_output_exists() {
        let scratch = Scratch::new("psd-file-size-preflight");
        let mut job = a_strip_job(&scratch, StripMode::Single);
        let source = &mut job.project.sources[2];
        source.w = 14_000;
        source.h = 14_000;
        source.mode = cleaner_core::image::ColorMode::Rgb;
        source.bit_depth = cleaner_core::image::BitDepth::Sixteen;
        let destination = scratch.join("psd-output");

        let result = export_open_job(
            &job, "PSD", destination.to_str().unwrap(), "separate-layer", "per-page",
        ).unwrap();
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.refusedLayeredFileSize"));
        assert!(!destination.exists());
    }

    #[test]
    fn invisible_patch_with_missing_sidecar_is_ignored_by_all_export_paths() {
        let scratch = Scratch::new("invisible-missing-sidecar");
        let mut job = a_strip_job(&scratch, StripMode::Longstrip);
        overhanging_fill(&mut job, 1, Rect::new(10, 10, 8, 8), 200);
        job.project.patches[0].visible = false;
        let buffer = job.sidecar().join(&job.project.patches[0].buffer_ref);
        std::fs::remove_file(buffer).unwrap();
        let strip = strip_of(&job);

        assert!(owned_patches(&job, 1).unwrap().is_empty());
        assert!(page_patches(&job, &strip, 1).unwrap().is_empty());
        assert!(global_patches(&job, &strip).unwrap().is_empty());
        let destination = scratch.join("pages");
        let result = export_open_job(
            &job, "PNG", destination.to_str().unwrap(), "flattened", "per-page",
        ).unwrap();
        assert_eq!(result.file_count, Some(3));
    }

    /// The seam takes an absolute path, and refuses a
    /// relative one rather than resolving it against a working directory
    /// neither end of the seam chose.
    #[test]
    fn an_absolute_destination_is_written_to_and_a_relative_one_is_refused() {
        let scratch = Scratch::new("destination");
        let job = a_job(&scratch);
        let elsewhere = scratch.join("somewhere/else");

        let result = export(&job, "PNG", elsewhere.to_str().unwrap());
        assert_eq!(result.status, "exported");
        assert_eq!(result.file_count, Some(1));
        assert_eq!(PathBuf::from(result.path.unwrap()), elsewhere.canonicalize().unwrap());
        assert!(elsewhere.join("001.png").exists());

        let refused = export(&job, "PNG", "../up/one");
        assert_eq!(refused.status, "refused");
        assert_eq!(refused.reason_key, Some("notice.export.refusedDestination"));
    }

    /// A chapter imported from a drive that is no longer connected. The pages
    /// are in the job's own sidecar and the origin is a path on the drive, so
    /// the default destination sits under a mount point that does not exist,
    /// inside a directory this process cannot write to. `/Volumes` is that
    /// directory on macOS; a read-only one stands in for it here.
    #[cfg(unix)]
    fn a_job_from_a_missing_drive(scratch: &Scratch) -> (Job, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let mut job = a_job(scratch);
        let volumes = scratch.join("volumes");
        std::fs::create_dir_all(&volumes).unwrap();
        std::fs::set_permissions(&volumes, std::fs::Permissions::from_mode(0o555)).unwrap();
        job.project.sources[0].converted_from = Some(volumes.join("Portable/chapter/001.jpg"));
        (job, volumes)
    }

    #[cfg(unix)]
    fn unlock(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The refusal is decided in the plan, names the folder, and creates
    /// nothing. The export itself answers the same way, not with a bare
    /// `Permission denied (os error 13)`.
    #[cfg(unix)]
    #[test]
    fn a_destination_on_a_missing_drive_is_refused_with_its_path_and_writes_nothing() {
        let scratch = Scratch::new("missing-drive");
        let (job, volumes) = a_job_from_a_missing_drive(&scratch);
        let expected = volumes.join("Portable/chapter_cleaned");

        let plan = plan_open_job(&job, "PNG", "new-folder", "flattened", "per-page").unwrap();
        let result = export_open_job(&job, "PNG", "new-folder", "flattened", "per-page");
        let created = volumes.join("Portable").exists();
        unlock(&volumes);

        assert_eq!(plan.status, "refused");
        assert_eq!(plan.reason_key, Some("notice.export.destinationUnavailable"));
        assert_eq!(plan.path.as_deref(), Some(expected.to_str().unwrap()));
        let result = result.unwrap();
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.destinationUnavailable"));
        assert!(!created, "a refused export created part of its destination");
        let (key, params, _) = announcement(&result, "PNG").unwrap();
        assert_eq!(key, "notice.export.destinationUnavailable");
        assert_eq!(params["path"], expected.to_str().unwrap());
    }

    /// The way out the dialog offers: the same chapter, a folder the user
    /// names.
    #[cfg(unix)]
    #[test]
    fn a_job_from_a_missing_drive_exports_to_an_absolute_folder() {
        let scratch = Scratch::new("missing-drive-elsewhere");
        let (job, volumes) = a_job_from_a_missing_drive(&scratch);
        let elsewhere = scratch.join("chosen/folder");

        let result = export(&job, "PNG", elsewhere.to_str().unwrap());
        unlock(&volumes);

        assert_eq!(result.status, "exported");
        assert_eq!(result.file_count, Some(1));
        assert!(elsewhere.join("001.png").exists());
    }

    /// A destination whose nearest existing ancestor is a file cannot be
    /// created either, and a folder that only has to be made can.
    #[test]
    fn availability_is_decided_from_the_nearest_ancestor_that_exists() {
        let scratch = Scratch::new("available");
        assert!(destination_available(&scratch.join("not/yet/made")));
        let file = scratch.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        assert!(!destination_available(&file.join("under")));
        assert!(!scratch.join("not").exists(), "the check created a folder");
    }

    /// An absolute path is still put to `Job::output_refusal`: naming the
    /// source directory the long way round is the same request.
    #[test]
    fn an_absolute_path_at_the_source_folder_is_refused_like_the_sentinel() {
        let scratch = Scratch::new("absolute-source");
        let job = a_job(&scratch);
        let raws = scratch.join("raws");

        let result = export(&job, "PNG", raws.to_str().unwrap());
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.refusedOverwrite"));
    }

    /* -------------------------------------------------------------- */
    /* CBZ                                                             */
    /* -------------------------------------------------------------- */

    /// A CBZ is the per-page files inside a zip, and the pages in it are the
    /// *same bytes* the per-page export writes - which is the whole claim, and
    /// the reason the container is not an encoder. Asserted against a second
    /// export rather than against a re-encode.
    #[test]
    fn a_cbz_holds_exactly_what_the_per_page_export_writes() {
        let scratch = Scratch::new("cbz");
        let mut job = a_strip_job(&scratch, StripMode::Longstrip);
        overhanging_fill(&mut job, 0, Rect::new(10, 40, 20, 16), 200);

        let pages = export(&job, "PNG", scratch.join("pages").to_str().unwrap());
        assert_eq!(pages.file_count, Some(3));
        let pages_dir = PathBuf::from(pages.path.unwrap());

        let archive = export(&job, "CBZ", scratch.join("archive").to_str().unwrap());
        assert_eq!(archive.status, "exported");
        assert_eq!(archive.file_count, Some(3));
        let archive_path = PathBuf::from(archive.path.unwrap());
        assert_eq!(archive_path.extension().unwrap(), "cbz");
        assert_eq!(archive_path.file_name().unwrap(), "chapter.cbz");

        let entries = unzip(&std::fs::read(&archive_path).unwrap());
        assert_eq!(
            entries.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
            ["000.png", "001.png", "002.png"],
            "the archive is not the chapter in strip order"
        );
        for (name, bytes) in &entries {
            assert_eq!(
                bytes,
                &std::fs::read(pages_dir.join(name)).unwrap(),
                "{name} in the archive is not the file the per-page export wrote"
            );
        }
    }

    /// The archive's entries decode, and the page that nothing touched is still
    /// the source file byte-for-byte *inside the zip* - the strongest
    /// form of passthrough, reached through the container.
    #[test]
    fn an_untouched_page_survives_the_container_byte_for_byte() {
        let scratch = Scratch::new("cbz-passthrough");
        let mut job = a_strip_job(&scratch, StripMode::Longstrip);
        overhanging_fill(&mut job, 0, Rect::new(10, 40, 20, 16), 200);

        let result = export(&job, "CBZ", "new-folder");
        let entries = unzip(&std::fs::read(result.path.unwrap()).unwrap());
        let by_name = |name: &str| {
            entries.iter().find(|(entry, _)| entry == name).map(|(_, bytes)| bytes.clone()).unwrap()
        };

        let source = |n: usize| std::fs::read(job.source_path(n).unwrap()).unwrap();
        assert_eq!(by_name("002.png"), source(2), "a page nothing reaches was re-encoded");
        assert_ne!(by_name("000.png"), source(0), "the page the patch anchors on");
        assert_ne!(by_name("001.png"), source(1), "the page below the join");

        let below = cleaner_core::image::decode(&by_name("001.png")).unwrap();
        for x in 10..30 {
            assert_eq!(below.sample(x, 0, 0), 200, "column {x} of the page below the join");
        }
    }

    /// Reads an archive the way an unpacker does: end record, central
    /// directory, local headers. The writer's own bookkeeping is not consulted.
    fn unzip(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
        let end = archive.len() - 22;
        assert_eq!(&archive[end..end + 4], &[0x50, 0x4b, 0x05, 0x06], "no end-of-directory record");
        let count = u16::from_le_bytes(archive[end + 10..end + 12].try_into().unwrap()) as usize;
        let mut at = u32::from_le_bytes(archive[end + 16..end + 20].try_into().unwrap()) as usize;

        let mut out = Vec::new();
        for _ in 0..count {
            let size = u32::from_le_bytes(archive[at + 24..at + 28].try_into().unwrap()) as usize;
            let name_len = u16::from_le_bytes(archive[at + 28..at + 30].try_into().unwrap()) as usize;
            let local = u32::from_le_bytes(archive[at + 42..at + 46].try_into().unwrap()) as usize;
            let name = String::from_utf8(archive[at + 46..at + 46 + name_len].to_vec()).unwrap();
            at += 46 + name_len;
            let local_name =
                u16::from_le_bytes(archive[local + 26..local + 28].try_into().unwrap()) as usize;
            let extra =
                u16::from_le_bytes(archive[local + 28..local + 30].try_into().unwrap()) as usize;
            let data = local + 30 + local_name + extra;
            out.push((name, archive[data..data + size].to_vec()));
        }
        out
    }

    /* -------------------------------------------------------------- */
    /* Stitched                                                        */
    /* -------------------------------------------------------------- */

    /// The writer was built, tested and unreachable. This is the
    /// seam parameter reaching it - one file, the strip's dimensions, and the
    /// patch that spans a join landing across the join inside it.
    #[test]
    fn a_stitched_export_writes_one_file_the_height_of_the_strip() {
        let scratch = Scratch::new("stitched");
        let mut job = a_strip_job(&scratch, StripMode::Longstrip);
        overhanging_fill(&mut job, 0, Rect::new(10, 40, 20, 16), 200);

        let result =
            export_open_job(&job, "PNG", "new-folder", "flattened", "stitched").unwrap();
        assert_eq!(result.status, "exported");
        assert_eq!(result.file_count, Some(1), "a stitched export is one file");
        assert_eq!(result.gutter_pixels, Some(0), "three pages of one width have no gutter");

        let path = PathBuf::from(result.path.unwrap());
        assert_eq!(path.file_name().unwrap(), "chapter.png");
        let strip = strip_of(&job);
        let written = cleaner_core::image::decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!((written.width, written.height), (strip.width(), strip.height()));

        // The join at strip row 48, from inside one file: the fill is
        // continuous across it.
        for x in 10..30 {
            assert_eq!(written.sample(x, 47, 0), 200, "column {x} above the join");
            assert_eq!(written.sample(x, 48, 0), 200, "column {x} below the join");
        }
    }

    /// A paginated project has no strip. Stacking its pages into one tall file
    /// would invent a document nobody has, so it is refused rather than
    /// offered - and the interface does not offer it either.
    #[test]
    fn a_paginated_project_cannot_be_stitched() {
        let scratch = Scratch::new("stitch-single");
        let job = a_strip_job(&scratch, StripMode::Single);

        let result = export_open_job(&job, "PNG", "new-folder", "flattened", "stitched").unwrap();
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.refusedStitchPaginated"));
        assert!(std::fs::read_dir(scratch.join("raws_cleaned")).is_err(), "wrote anyway");
    }

    /// The strip's own refusals reach the seam as refusals with the reason on
    /// them, rather than as an error string the interface would have to
    /// translate. Page 1 is RGB where the strip is grayscale.
    #[test]
    fn a_strip_whose_pages_disagree_refuses_with_the_reason_on_it() {
        let scratch = Scratch::new("stitch-mixed");
        let raws = scratch.join("raws");
        let sources = vec![
            write_source(&raws, "000.png", "l8"),
            write_source(&raws, "001.png", "rgb8"),
            write_source(&raws, "002.png", "l8"),
        ];
        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project =
            Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Longstrip, &sources);
        let job = Job::create(&manifest, project).unwrap();

        let result = export_open_job(&job, "PNG", "new-folder", "flattened", "stitched").unwrap();
        assert_eq!(result.status, "refused");
        assert_eq!(result.reason_key, Some("notice.export.stitchRefused.mixedMode"));
        assert!(
            !scratch.join("raws_cleaned/chapter.png").exists(),
            "a refused stitch left the file it opened"
        );
    }

    /// The gutter is counted, and the count is on the
    /// seam, which is where the interface reads it back from. Page 1 is
    /// narrower than the strip, so it is the one with white beside it.
    #[test]
    fn a_ragged_strip_reports_the_pixels_no_source_page_had() {
        let scratch = Scratch::new("stitch-gutter");
        let raws = scratch.join("raws");
        let sources = vec![
            write_source(&raws, "000.png", "l8"),
            write_narrow_source(&raws, "001.png", 40),
            write_source(&raws, "002.png", "l8"),
        ];
        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project =
            Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Longstrip, &sources);
        let job = Job::create(&manifest, project).unwrap();

        let strip = strip_of(&job);
        let expected: u64 = strip
            .pages()
            .iter()
            .map(|page| u64::from(strip.width() - page.width) * u64::from(page.height))
            .sum();
        assert!(expected > 0, "the fixture is not ragged");

        let result = export_open_job(&job, "PNG", "new-folder", "flattened", "stitched").unwrap();
        assert_eq!(result.gutter_pixels, Some(expected));
    }

    /// A CBZ of one stitched file is a shape nobody asked for, and packing it
    /// would mean holding the strip's encoded file whole.
    #[test]
    fn a_stitched_archive_is_refused() {
        let none = MaskFiles::None;
        let beside = MaskFiles::Beside;
        assert_eq!(
            container_for(Requested::Cbz, "stitched", none, true),
            Err("notice.export.refusedStitchedArchive")
        );
        assert_eq!(container_for(Requested::Cbz, "per-page", none, true), Ok(Container::Archive));
        assert_eq!(
            container_for(Requested::Cbz, "per-page", beside, true),
            Err("notice.export.refusedMaskLayers")
        );
        // An unrecognised layout is rule 8's default rather than a refusal:
        // "one file per page" is what every export did before the parameter.
        assert_eq!(
            container_for(Requested::Png, "sideways", none, true),
            Ok(Container::Pages { target: Target::Explicit(Format::Png), masks: none })
        );
        assert_eq!(
            container_for(Requested::Tiff, "stitched", beside, true),
            Ok(Container::Stitched { target: Target::Explicit(Format::Tiff), masks: beside })
        );
        assert_eq!(
            container_for(Requested::Psd, "per-page", beside, false),
            Ok(Container::Layered { layered: true })
        );
        assert_eq!(
            container_for(Requested::Psd, "per-page", none, false),
            Ok(Container::Layered { layered: false })
        );
        assert_eq!(
            container_for(Requested::Psd, "stitched", none, true),
            Err("notice.export.refusedStitchedLayered")
        );
    }

    /// The strip's mask file: one PNG the strip's size, from the same global
    /// patch set in the same coordinates, so the fill that spans the join is
    /// white on both sides of it.
    #[test]
    fn a_stitched_export_writes_the_strips_mask_beside_it() {
        let scratch = Scratch::new("stitched-mask");
        let mut job = a_strip_job(&scratch, StripMode::Longstrip);
        overhanging_fill(&mut job, 0, Rect::new(10, 40, 20, 16), 200);

        let result =
            export_open_job(&job, "PNG", "new-folder", "separate-layer", "stitched").unwrap();
        assert_eq!(result.status, "exported");
        let path = PathBuf::from(result.path.unwrap());
        let mask_path = path.with_file_name("chapter_mask.png");
        let mask = cleaner_core::image::decode(&std::fs::read(&mask_path).unwrap()).unwrap();
        assert_eq!((mask.width, mask.height), (64, 144));
        for y in 0..144u32 {
            for x in 0..64u32 {
                let inside = (10..30).contains(&x) && (40..56).contains(&y);
                assert_eq!(mask.sample(x, y, 0), if inside { 255 } else { 0 }, "({x}, {y})");
            }
        }
    }

    /* -------------------------------------------------------------- */
    /* A real chapter, in every shape this build offers                */
    /* -------------------------------------------------------------- */

    /// **The four writers, over `fixtures/pages/strip-01…03.png`.**
    ///
    /// Every other test here runs on the 64×48 synthetic fixtures, which is
    /// right for asserting a rule and wrong for the claim "this build can
    /// export a chapter": three 800×4000 pages is a chapter, a 12 000-row
    /// stitched file is a strip, and neither shape is exercised by a page the
    /// size of a postage stamp. What is checked of each written file is what a
    /// user would check - that it opens, that it is the size it should be, and
    /// that its mode, depth and profile are the source's - plus
    /// the subset assertion, which is the one that fails on any conversion.
    #[test]
    fn a_real_chapter_exports_to_png_tiff_cbz_and_one_stitched_file() {
        let scratch = Scratch::new("real-chapter");
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let pages_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/pages");

        let sources: Vec<SourceRef> = (1..=3)
            .map(|n| {
                let bytes = std::fs::read(pages_dir.join(format!("strip-{n:02}.png"))).unwrap();
                let path = raws.join(format!("{n:03}.png"));
                std::fs::write(&path, &bytes).unwrap();
                cleaner_core::ingest::source_ref(&path, &bytes).unwrap()
            })
            .collect();
        assert_eq!(
            (sources[0].width, sources[0].height),
            (800, 4000),
            "the fixture is not the strip"
        );

        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project =
            Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Longstrip, &sources);
        let mut job = Job::create(&manifest, project).unwrap();
        // A fill that runs 40 rows past page 0's bottom edge, so the join at
        // strip row 4000 is crossed and pages 0 and 1 both have work in them.
        overhanging_fill(&mut job, 0, Rect::new(120, 3960, 200, 80), 40);

        let source = |n: usize| {
            cleaner_core::image::decode(&std::fs::read(job.source_path(n).unwrap()).unwrap())
                .unwrap()
        };
        let strip = strip_of(&job);

        /* PNG, per page ------------------------------------------------ */
        let png = export(&job, "PNG", scratch.join("png").to_str().unwrap());
        assert_eq!(png.status, "exported");
        assert_eq!(png.file_count, Some(3));
        let png_dir = PathBuf::from(png.path.unwrap());
        for position in 0..3 {
            let before = source(position);
            let path = png_dir.join(format!("{:03}.png", position + 1));
            let after =
                cleaner_core::image::decode(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!((after.width, after.height), (800, 4000), "page {position}: dimensions");
            assert_eq!(after.mode, before.mode, "page {position}: mode");
            assert_eq!(after.depth, before.depth, "page {position}: bit depth");
            assert_eq!(after.icc, before.icc, "page {position}: icc profile");

            // The subset assertion, against the file on disk.
            let patches = page_patches(&job, &strip, position).unwrap();
            let permitted = permitted_region(&patches, before.width, before.height);
            for (x, y) in changed_pixels(&before, &after) {
                assert!(
                    permitted.contains(x as i64, y as i64),
                    "page {position}: ({x}, {y}) changed outside the permitted region"
                );
            }
        }
        assert_ne!(
            std::fs::read(png_dir.join("002.png")).unwrap(),
            std::fs::read(job.source_path(1).unwrap()).unwrap(),
            "the page below the join kept nothing of the patch that spans it"
        );

        /* TIFF, per page ----------------------------------------------- */
        let tiff = export(&job, "TIFF", scratch.join("tiff").to_str().unwrap());
        assert_eq!(tiff.file_count, Some(3));
        let tiff_dir = PathBuf::from(tiff.path.unwrap());
        for position in 0..3 {
            let before = source(position);
            let path = tiff_dir.join(format!("{:03}.tiff", position + 1));
            let after =
                cleaner_core::image::decode(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!((after.width, after.height), (800, 4000), "page {position}: dimensions");
            assert_eq!(after.mode, before.mode, "page {position}: mode");
            assert_eq!(after.depth, before.depth, "page {position}: bit depth");

            let patches = page_patches(&job, &strip, position).unwrap();
            let permitted = permitted_region(&patches, before.width, before.height);
            for (x, y) in changed_pixels(&before, &after) {
                assert!(
                    permitted.contains(x as i64, y as i64),
                    "page {position} as TIFF: ({x}, {y}) changed outside the permitted region"
                );
            }
        }

        /* CBZ ----------------------------------------------------------- */
        let cbz = export(&job, "CBZ", scratch.join("cbz").to_str().unwrap());
        assert_eq!(cbz.file_count, Some(3));
        let archive_path = PathBuf::from(cbz.path.unwrap());
        let entries = unzip(&std::fs::read(&archive_path).unwrap());
        assert_eq!(
            entries.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
            ["001.png", "002.png", "003.png"]
        );
        for (name, bytes) in &entries {
            assert_eq!(
                bytes,
                &std::fs::read(png_dir.join(name)).unwrap(),
                "{name} inside the archive is not the file the per-page export wrote"
            );
        }

        /* One file for the chapter --------------------------------------- */
        let stitched = export_open_job(
            &job,
            "PNG",
            scratch.join("stitched").to_str().unwrap(),
            "flattened",
            "stitched",
        )
        .unwrap();
        assert_eq!(stitched.status, "exported", "{stitched:?}");
        assert_eq!(stitched.file_count, Some(1));
        assert_eq!(stitched.gutter_pixels, Some(0), "three 800-wide pages have no gutter");
        let one_file =
            cleaner_core::image::decode(&std::fs::read(stitched.path.unwrap()).unwrap()).unwrap();
        assert_eq!((one_file.width, one_file.height), (800, 12_000));
        assert_eq!(one_file.mode, source(0).mode);
        assert_eq!(one_file.depth, source(0).depth);

        // The join at strip row 4000, from inside the one file: the fill that
        // spans it is one tone on both sides of it.
        for x in 120..320 {
            assert_eq!(one_file.sample(x, 3999, 0), 40, "column {x} above the join");
            assert_eq!(one_file.sample(x, 4000, 0), 40, "column {x} below the join");
        }
        // And a row nothing touched is the source's own row.
        let last = source(2);
        for x in (0..800).step_by(37) {
            assert_eq!(
                one_file.sample(x, 8000 + 2500, 0),
                last.sample(x, 2500, 0),
                "column {x} of the last page was not carried through"
            );
        }
    }


    /* -------------------------------------------------------------- */
    /* The notice stack                                                */
    /* -------------------------------------------------------------- */

    /// Both statuses are announced by name, and a status this build does not
    /// know is announced not at all. The arm that used to be `_` would have
    /// said *"Exported N files"* over a third outcome - a partial write, a
    /// cancelled export - in the voice of a thing that finished.
    ///
    /// And a refusal announces **its own** reason. The arm named
    /// `notice.export.refusedOverwrite` outright while the overwrite was the
    /// only refusal there was; seven refusals later that would tell a user who
    /// asked for JPEG that their source files were nearly replaced.
    #[test]
    fn every_outcome_announces_itself_and_no_outcome_announces_another() {
        let exported = ExportResult {
            status: "exported",
            file_count: Some(3),
            path: Some("/out".into()),
            reason_key: None,
            gutter_pixels: None,
            actual_formats: Vec::new(), declared: Vec::new(), outputs: Vec::new(), revision: None, detail: None,
        };
        let (key, params, tone) = announcement(&exported, "PNG").expect("no announcement");
        assert_eq!(key, "notice.export.finished");
        assert_eq!(params["count"], 3);
        assert_eq!(params["format"], "PNG");
        assert_eq!(tone, "info");

        let refused =
            ExportResult::refused("notice.export.refusedOverwrite", Some("/out".into()));
        let (key, params, tone) = announcement(&refused, "PNG").expect("no announcement");
        assert_eq!(key, "notice.export.refusedOverwrite");
        assert_eq!(params["path"], "/out");
        assert_eq!(tone, "warn");

        let lossy = ExportResult::refused("notice.export.refusedLossyFormat", None);
        let (key, params, _) = announcement(&lossy, "JPEG").expect("no announcement");
        assert_eq!(key, "notice.export.refusedLossyFormat", "a refusal announced another's reason");
        assert_eq!(params["format"], "JPEG");

        let stitched = ExportResult {
            status: "exported",
            file_count: Some(1),
            path: Some("/out/chapter.png".into()),
            reason_key: None,
            gutter_pixels: Some(1536),
            actual_formats: Vec::new(), declared: Vec::new(), outputs: Vec::new(), revision: None, detail: None,
        };
        let (key, params, _) = announcement(&stitched, "PNG").expect("no announcement");
        assert_eq!(key, "notice.export.stitched");
        assert_eq!(params["count"], 1536, "the gutter count is not in the sentence that states it");

        let unknown = ExportResult { status: "unknown-future-status", ..exported };
        assert!(
            announcement(&unknown, "PNG").is_none(),
            "a status this build does not know announced that the export finished"
        );
    }

    /* -------------------------------------------------------------- */
    /* The geometry, unchanged                                         */
    /* -------------------------------------------------------------- */

    /// **The intersection, through a real manifest.**
    ///
    /// The patch is anchored on page 0 and runs 8 rows past its bottom edge.
    /// Page 1 owns no patch - `PatchRecord.source_idx` says 0, and it is the
    /// only page-ownership fact a manifest holds - so an exporter that selects
    /// by ownership writes page 1 byte-for-byte and loses the bottom of the
    /// bubble. The intersection writes both.
    #[test]
    fn a_longstrip_patch_that_spans_a_join_reaches_both_pages_files() {
        let scratch = Scratch::new("join");
        let mut job = a_strip_job(&scratch, StripMode::Longstrip);
        overhanging_fill(&mut job, 0, Rect::new(10, 40, 20, 16), 200);

        let result = export(&job, "PNG", "new-folder");
        assert_eq!(result.file_count, Some(3));
        let out = PathBuf::from(result.path.unwrap());

        let source = |n: usize| std::fs::read(job.source_path(n).unwrap()).unwrap();
        let written = |n: usize| std::fs::read(out.join(format!("{n:03}.png"))).unwrap();

        assert_ne!(written(0), source(0), "page 0 lost the part it anchors");
        assert_ne!(written(1), source(1), "page 1 was declared untouched and is not");
        assert_eq!(written(2), source(2), "a page nothing reaches was re-encoded");

        // The seam, from the two files: page 0's last row and page 1's first
        // row are adjacent rows of one fill, and they carry one tone.
        let above = cleaner_core::image::decode(&written(0)).unwrap();
        let below = cleaner_core::image::decode(&written(1)).unwrap();
        for x in 10..30 {
            assert_eq!(above.sample(x, 47, 0), 200, "page 0 column {x}");
            assert_eq!(below.sample(x, 0, 0), 200, "page 1 column {x}");
        }
        assert_eq!(below.sample(9, 0, 0), source_raster(&job, 1).sample(9, 0, 0));

        // The subset assertion against the written files, page by page, over
        // the parts the intersection actually gave each page.
        let strip = strip_of(&job);
        for position in 0..3 {
            let before = source_raster(&job, position);
            let after = cleaner_core::image::decode(&written(position)).unwrap();
            assert_eq!(after.mode, before.mode, "page {position}: mode");
            assert_eq!(after.depth, before.depth, "page {position}: bit depth");
            assert_eq!(after.icc, before.icc, "page {position}: icc profile");

            let patches = page_patches(&job, &strip, position).unwrap();
            let permitted = permitted_region(&patches, before.width, before.height);
            for (x, y) in changed_pixels(&before, &after) {
                assert!(
                    permitted.contains(x as i64, y as i64),
                    "page {position}: ({x}, {y}) changed outside the permitted region"
                );
            }
        }
    }

    /// The same manifest in `Single` mode. There is no strip, so there is no
    /// join for a patch to span: a bbox running past the page is a malformed
    /// patch, clipped at that page's edge, and it must not appear in the next
    /// file. Splitting unconditionally would spill one page's edit into another
    /// image that has nothing to do with it.
    #[test]
    fn a_paginated_job_never_spills_a_patch_into_the_next_file() {
        let scratch = Scratch::new("single");
        let mut job = a_strip_job(&scratch, StripMode::Single);
        overhanging_fill(&mut job, 0, Rect::new(10, 40, 20, 16), 200);

        let result = export(&job, "PNG", "new-folder");
        let out = PathBuf::from(result.path.unwrap());
        let source = |n: usize| std::fs::read(job.source_path(n).unwrap()).unwrap();
        let written = |n: usize| std::fs::read(out.join(format!("{n:03}.png"))).unwrap();

        assert_ne!(written(0), source(0), "the page the patch is on");
        assert_eq!(written(1), source(1), "an edit crossed into the next file");
        assert_eq!(written(2), source(2));
    }

    /// Selection is from the manifest's rectangles, and only the records that
    /// survive it are read off disk. Loading the global set first would put
    /// every patch in the chapter in RAM to answer a question about one page -
    /// which is what keeping them on disk avoids.
    #[test]
    fn a_page_is_selected_from_bboxes_before_any_buffer_is_loaded() {
        let scratch = Scratch::new("select");
        let mut job = a_strip_job(&scratch, StripMode::Longstrip);
        overhanging_fill(&mut job, 0, Rect::new(10, 40, 20, 16), 200);
        // A second patch, wholly inside page 2 and nowhere near the first.
        let mut second = fixtures::by_name("l8").raster;
        second.width = 4;
        second.height = 4;
        second.icc = None;
        second.data = vec![9; 16];
        job.complete_region(
            2,
            &Patch {
                id: "on-page-2".into(),
                mask: Mask::filled(Rect::new(2, 2, 4, 4)),
                ink: Mask::filled(Rect::new(2, 2, 4, 4)),
                pixels: second,
                order: 0,
                visible: true,
                provenance: provenance(),
            },
            None,
        )
        .unwrap();

        let strip = strip_of(&job);
        let ids = |position: usize| {
            intersecting_records(&job, &strip, position)
                .into_iter()
                .map(|(_, record)| record.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(0), ["across-the-join"]);
        assert_eq!(ids(1), ["across-the-join"], "the page it does not belong to");
        assert_eq!(ids(2), ["on-page-2"], "a patch two pages away was loaded for this one");
        let distant = job.project.patches.iter().find(|record| record.id == "on-page-2").unwrap();
        std::fs::remove_file(job.sidecar().join(&distant.buffer_ref)).unwrap();
        let patches = page_patches_for(&job, 0).unwrap();
        assert_eq!(patches.len(), 1, "per-page export loaded a distant sidecar");
    }

    #[test]
    fn export_selects_a_moved_or_rotated_patch_on_its_destination_page() {
        for angle in [0.0, 90.0] {
            let scratch = Scratch::new(if angle == 0.0 { "export-moved" } else { "export-rotated" });
            let mut job = a_strip_job(&scratch, StripMode::Longstrip);
            overhanging_fill(&mut job, 0, Rect::new(10, 10, 20, 10), 200);
            job.project.patches[0].provenance.params_snapshot["layer"] =
                serde_json::json!({"offsetY": 40, "rotation": angle});
            let patches = page_patches_for(&job, 1).unwrap();
            assert_eq!(patches.len(), 1, "angle {angle} was skipped by export selection");
            assert!(patches[0].mask.contains(20, 5));
            let bounds = patches[0].mask.bounds;
            assert_eq!(patches[0].pixels.sample((20 - bounds.x) as u32, (5 - bounds.y) as u32, 0), 200);
        }
    }

    fn source_raster(job: &Job, index: usize) -> cleaner_core::image::Raster {
        cleaner_core::image::decode(&std::fs::read(job.source_path(index).unwrap()).unwrap())
            .unwrap()
    }
}
