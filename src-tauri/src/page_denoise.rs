//! Page denoise on this computer: the three commands the interface calls.
//!
//! - [`denoise_chapter_local`] denoises a chapter's pages with a local preset
//!   into `<out_dir>/<source stem>.png` and answers the same report a cloud
//!   denoise does ([`CloudDenoiseReport`]). While it runs it emits
//!   `denoise://progress` ([`DenoiseProgress`]), and [`cancel_denoise_local`]
//!   stops it at the next tile.
//! - [`benchmark_denoise_local`] times one synthetic page, so the interface
//!   can say how long a chapter would take here.
//! - [`denoise_presets`] lists the shipped presets and where each can run.
//! - [`replace_with_denoised`] takes the files a finished denoise wrote as the
//!   chapter's pages, keeping every detection, and [`replacement`] tells the
//!   chapter list whether to offer it.
//!
//! A page is read exactly as the cloud path reads it
//! ([`cloud_denoise::cleaned_composite`]: the source and its visible patches),
//! run through [`cleaner_core::page_denoise`], and written atomically. A page
//! that fails is reported and the run goes on. Nothing here is sent anywhere.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use cleaner_core::cloud_denoise_wire::{self as wire, DenoiseRecipe, PresetTarget};
use cleaner_core::image::{decode, encode, BitDepth, ColorMode, Format, Raster};
use cleaner_core::page_denoise::{self as engine, Denoised, LocalStep, Waifu2x};
use cleaner_core::project::{Job, Project, ReplaceError};
use serde::Serialize;

use crate::inference::cloud_denoise::{
    cleaned_composite, output_names, record_written_soon, write_atomic, CleanedError, CloudDenoiseReport, DenoisedOutput,
    PageFailure, PlannedPage, MAX_PLAN_PAGES,
};
use crate::library::Library;

/// The per-model accelerator key (`modelAccelerators.pageDenoise`), falling
/// back to the global `accelerator` setting like every other model.
pub const MODEL_ACCELERATOR_ID: &str = "pageDenoise";

/// The synthetic benchmark page: the size of the real scans it was measured on.
const BENCH_WIDTH: u32 = 1284;
const BENCH_HEIGHT: u32 = 1809;

/// The event a local run emits after each tile and each page.
pub const PROGRESS_EVENT: &str = "denoise://progress";

/// How far a local run is.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DenoiseProgress {
    pub run_id: String,
    /// Pages finished, written or failed.
    pub done: usize,
    pub total: usize,
    /// How far through the page being denoised, 0 to 1.
    pub page: f64,
}

/// Which denoise a registered run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DenoiseKind {
    Local,
    Cloud,
}

impl DenoiseKind {
    fn as_str(self) -> &'static str {
        match self {
            DenoiseKind::Local => "denoise",
            DenoiseKind::Cloud => "cloudDenoise",
        }
    }
}

/// One denoise run in flight, local or cloud: what `list_jobs` shows of it
/// and the flag [`cancel_denoise_local`] raises.
pub(crate) struct DenoiseRun {
    kind: DenoiseKind,
    chapter_id: String,
    done: AtomicUsize,
    total: AtomicUsize,
    cancel: AtomicBool,
}

/// Each denoise run in flight, local and cloud, by the run id the interface
/// chose (or the backend minted, for a caller that named none).
fn runs() -> &'static Mutex<HashMap<String, Arc<DenoiseRun>>> {
    static RUNS: OnceLock<Mutex<HashMap<String, Arc<DenoiseRun>>>> = OnceLock::new();
    RUNS.get_or_init(Default::default)
}

/// A run's registry entry, held while it runs and forgotten when it ends.
pub(crate) struct Registered(String, Arc<DenoiseRun>);

impl Registered {
    pub(crate) fn new(run_id: &str, kind: DenoiseKind, chapter_id: &str) -> Result<Registered, String> {
        if run_id.is_empty() || run_id.len() > 64 { return Err("denoise_run_id_invalid".into()); }
        let mut map = runs().lock().map_err(|e| e.to_string())?;
        if map.contains_key(run_id) { return Err("denoise_run_id_invalid".into()); }
        let run = Arc::new(DenoiseRun {
            kind, chapter_id: chapter_id.to_owned(), done: AtomicUsize::new(0), total: AtomicUsize::new(0),
            cancel: AtomicBool::new(false),
        });
        map.insert(run_id.to_string(), run.clone());
        Ok(Registered(run_id.to_string(), run))
    }

    pub(crate) fn cancel(&self) -> &AtomicBool {
        &self.1.cancel
    }

    /// Where the run is, for `list_jobs`.
    pub(crate) fn progress(&self, done: usize, total: usize) {
        self.1.done.store(done, Ordering::Relaxed);
        self.1.total.store(total, Ordering::Relaxed);
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        if let Ok(mut map) = runs().lock() { map.remove(&self.0); }
    }
}

/// A run id for a caller that named none, in the interface's own shape
/// (`den-<ms>-<suffix>`), so it cannot be one the interface minted.
pub(crate) fn mint_run_id() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis());
    format!("den-{ms}-b{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Every denoise run in flight, for `list_jobs`.
pub(crate) fn jobs() -> Vec<crate::jobs::JobView> {
    let map = runs().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut jobs: Vec<crate::jobs::JobView> = map.iter().map(|(run_id, run)| {
        let clamp = |count: usize| u32::try_from(count).unwrap_or(u32::MAX);
        let total = run.total.load(Ordering::Relaxed);
        crate::jobs::JobView {
            run_id: run_id.clone(), kind: run.kind.as_str(), chapter_id: run.chapter_id.clone(),
            done: clamp(run.done.load(Ordering::Relaxed).min(total)), total: clamp(total),
        }
    }).collect();
    jobs.sort_by(|a, b| a.run_id.cmp(&b.run_id));
    jobs
}

/// Raise every denoise run's stop flag and answer how many there were.
pub(crate) fn cancel_all() -> usize {
    let map = runs().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for run in map.values() { run.cancel.store(true, Ordering::Relaxed); }
    map.len()
}

/// One preset as the interface lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetView {
    pub id: &'static str,
    pub targets: &'static [PresetTarget],
    pub recipe: DenoiseRecipe,
}

pub fn presets() -> Vec<PresetView> {
    wire::PRESETS.iter().map(|preset| PresetView { id: preset.id, targets: preset.targets, recipe: preset.recipe() }).collect()
}

/// The step a local preset runs, or why it cannot run here.
fn local_preset(preset_id: &str) -> Result<LocalStep, String> {
    let preset = wire::preset(preset_id).ok_or("denoise_preset_unknown")?;
    if !preset.runs_locally() {
        return Err("denoise_preset_not_local".into());
    }
    engine::local_step(&preset.recipe()).map_err(|e| format!("denoise_preset_not_local: {e}"))
}

/// Open the preset's model on the user's provider, after checking both files
/// are installed and match their pinned digests.
fn open_engine(app: &tauri::AppHandle, step: &LocalStep) -> Result<Waifu2x, String> {
    use tauri::Manager;
    let app_data = app.path().app_data_dir().ok();
    let files = crate::weights::verified_group(app_data.as_deref(), crate::weights::PAGE_DENOISE_GROUP)
        .map_err(|e| format!("denoise_models_missing: {e}"))?;
    let [model, seams] = files.as_slice() else { return Err("denoise_models_missing".into()) };
    cleaner_core::runtime::find(app_data.as_deref())
        .and_then(|path| cleaner_core::runtime::load(&path))
        .map_err(|e| format!("denoise_runtime_unavailable: {e}"))?;
    let settings = crate::settings::read(app).unwrap_or(serde_json::Value::Null);
    let preference = crate::run::model_preference_from(&settings, MODEL_ACCELERATOR_ID)?;
    Waifu2x::open(model, seams, step.model, preference).map_err(|e| format!("{}: {e}", e.code()))
}

/// The pages to denoise, every page when `page_indices` is absent, in order.
/// A read, so without the lock a run on the chapter holds.
fn plan_pages(job_path: &Path, page_indices: Option<Vec<u32>>) -> Result<Vec<PlannedPage>, String> {
    let job = Job::open(job_path).map_err(|e| e.to_string())?;
    let count = job.project.strip.order.len() as u32;
    let mut positions = page_indices.unwrap_or_else(|| (0..count).collect());
    positions.sort_unstable();
    positions.dedup();
    if positions.is_empty() { return Err("denoise_no_pages".into()); }
    if positions.len() > MAX_PLAN_PAGES { return Err("denoise_too_many_pages".into()); }
    positions.into_iter().map(|page_index| {
        let source_idx = Library::resolve_page(&job.project, page_index as usize).ok_or("denoise_page_missing")?;
        let declared = job.project.sources.get(source_idx).ok_or("denoise_page_missing")?;
        Ok(PlannedPage {
            page_index, source_idx,
            appearance: crate::tile::page_appearance(&job.project, page_index as usize),
            width: declared.orientation.size(declared.w,declared.h).0, height: declared.orientation.size(declared.w,declared.h).1,
        })
    }).collect()
}

/// The engine for one page: the raster, and `tick(done, total)` asked before
/// each tile, which stops the page when it answers false.
type DenoiseFn<'a> = dyn FnMut(&Raster, &mut dyn FnMut(usize, usize) -> bool) -> Result<Denoised, engine::Error> + 'a;

/// Denoise each page in order into `out_dir`, reporting each. `read` gives a
/// page's PNG; `denoise` runs the engine. `progress(done, page)` hears each
/// tile and each finished page. Once `cancel` is set the page in flight stops
/// at its next tile, is in neither list, and no later page starts.
fn denoise_pages(
    pages: &[PlannedPage],
    names: &HashMap<u32, String>,
    out_dir: &Path,
    read: &dyn Fn(&PlannedPage) -> Result<Vec<u8>, String>,
    denoise: &mut DenoiseFn<'_>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, f64),
) -> CloudDenoiseReport {
    let mut report = CloudDenoiseReport::default();
    let mut taken = HashSet::new();
    for (at, page) in pages.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(at, 0.0);
        let mut tick = |done: usize, total: usize| {
            progress(at, done as f64 / total.max(1) as f64);
            !cancel.load(Ordering::Relaxed)
        };
        match denoise_one(page, names, &mut taken, out_dir, read, &mut |raster| denoise(raster, &mut tick)) {
            Ok(output) => report.written.push(output),
            Err(code) if code.starts_with("denoise_cancelled") => {
                report.cancelled = true;
                break;
            }
            Err(code) => report.failed.push(PageFailure { page_index: page.page_index, code }),
        }
        progress(at + 1, 0.0);
    }
    report
}

fn denoise_one(
    page: &PlannedPage,
    names: &HashMap<u32, String>,
    taken: &mut HashSet<String>,
    out_dir: &Path,
    read: &dyn Fn(&PlannedPage) -> Result<Vec<u8>, String>,
    denoise: &mut dyn FnMut(&Raster) -> Result<Denoised, engine::Error>,
) -> Result<DenoisedOutput, String> {
    let name = names.get(&page.page_index).ok_or("denoise_page_missing")?;
    if !taken.insert(name.clone()) { return Err("denoise_duplicate_output".into()); }
    let raster = decode(&read(page)?).map_err(|e| format!("denoise_page_unreadable: {e}"))?;
    let denoised = denoise(&raster).map_err(|e| format!("{}: {e}", e.code()))?;
    let png = encode(&denoised.raster, Format::Png).map_err(|e| format!("denoise_write_failed: {e}"))?;
    let path = out_dir.join(format!("{name}.png"));
    write_atomic(&path, &png).map_err(|e| format!("denoise_write_failed: {e}"))?;
    Ok(DenoisedOutput { page_index: page.page_index, path: path.display().to_string(), gray: denoised.gray })
}

fn run_chapter(
    app: &tauri::AppHandle,
    run_id: &str,
    chapter_id: &str,
    page_indices: Option<Vec<u32>>,
    preset_id: &str,
    out_dir: &Path,
) -> Result<CloudDenoiseReport, String> {
    // First, so a Stop pressed while the model opens is already heard.
    let registered = Registered::new(run_id, DenoiseKind::Local, chapter_id)?;
    let step = local_preset(preset_id)?;
    if !out_dir.is_absolute() { return Err("denoise_out_dir_invalid".into()); }
    std::fs::create_dir_all(out_dir).map_err(|_| "denoise_out_dir_unwritable")?;
    let job_path = Library::for_app(app).map_err(|e| e.to_string())?
        .resolve_chapter(chapter_id).map_err(|e| e.to_string())?;
    let pages = plan_pages(&job_path, page_indices)?;
    let names = output_names(&job_path, &pages)?;
    let mut engine = open_engine(app, &step)?;
    // Refused when the page moved since it was planned, as a cloud run does,
    // so the appearance recorded with the file is the one it was made from.
    let read = |page: &PlannedPage| cleaned_composite(&job_path, chapter_id, page.page_index, Some(&page.appearance)).map_err(|e| match e {
        CleanedError::Unreadable(detail) => format!("denoise_page_unreadable: {detail}"),
        CleanedError::Changed => "denoise_page_changed".to_string(),
    });
    let mut denoise = |raster: &Raster, tick: &mut dyn FnMut(usize, usize) -> bool| {
        engine.denoise_watched(raster, step.strength, tick)
    };
    let total = pages.len();
    registered.progress(0, total);
    let mut progress = |done: usize, page: f64| {
        use tauri::Emitter;
        registered.progress(done, total);
        let _ = app.emit(PROGRESS_EVENT, DenoiseProgress { run_id: run_id.to_string(), done, total, page });
    };
    let report = denoise_pages(&pages, &names, out_dir, &read, &mut denoise, registered.cancel(), &mut progress);
    record_written_soon(app, run_id, &job_path, &pages, &report, Some(preset_id), PresetTarget::Local);
    Ok(report)
}

/* ------------------------------------------------------------------ */
/* Taking the denoised files as the pages                              */
/* ------------------------------------------------------------------ */

/// Where one page stands for [`replace_with_denoised`], by its current
/// denoised file ([`Project::current_denoised`]); older runs are history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// No denoised file is recorded for it (a run that skipped or failed
    /// the page), the file is gone, or it was already taken as the page. It
    /// stays raw, as it is; the other pages can still be taken.
    Missing,
    /// Its file was made from its source as it is now, with nothing drawn on
    /// it, and nothing has cleaned the page since.
    Ready,
    /// It stays as it is: cleaned, or changed since it was denoised. A file
    /// made from a cleaned page has the cleaning in its pixels, and taking it
    /// as the source would draw every patch twice.
    Kept,
}

/// Each page of the chapter in reading order: its position, its source, and
/// where it stands.
fn standings(project: &Project) -> Vec<(u32, usize, Standing)> {
    crate::tile::page_appearances(project).iter().enumerate().filter_map(|(position, appearance)| {
        let source_idx = Library::resolve_page(project, position)?;
        let standing = match project.current_denoised(source_idx) {
            Some(row) if row.path.is_file() => {
                let bare = *appearance == crate::tile::source_appearance(project, position);
                if bare && row.appearance == *appearance && !project.has_cleaning(source_idx) {
                    Standing::Ready
                } else {
                    Standing::Kept
                }
            }
            _ => Standing::Missing,
        };
        Some((position as u32, source_idx, standing))
    }).collect()
}

/// What the chapter list offers: at least one page has a file from a denoise
/// run still on disk that can be taken as its page. The rest stay as they
/// are, and the confirmation says how many and why.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DenoiseReplacement {
    /// Pages that would be replaced.
    pub pages: u32,
    /// Pages that would stay as they are: cleaned, or changed since denoise.
    pub kept: u32,
    /// Pages that would stay raw because they have no denoised file: never
    /// denoised, failed in the run, or their file is gone.
    pub missing: u32,
}

/// The offer for one chapter's manifest, or `None` when there is none.
/// Costs nothing for a chapter that was never denoised.
pub(crate) fn replacement(project: &Project) -> Option<DenoiseReplacement> {
    if project.denoised.is_empty() { return None; }
    let standings = standings(project);
    let count = |wanted: Standing| standings.iter().filter(|(_, _, standing)| *standing == wanted).count() as u32;
    let pages = count(Standing::Ready);
    (pages > 0).then(|| DenoiseReplacement { pages, kept: count(Standing::Kept), missing: count(Standing::Missing) })
}

/// What [`replace_with_denoised`] did, by page index. When any page fails,
/// nothing is saved: `failed` is not empty and `replaced` is.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceReport {
    pub replaced: Vec<u32>,
    /// Cleaned, or changed since denoise.
    pub kept: Vec<u32>,
    /// No denoised file to take: never denoised, failed, or gone.
    pub missing: Vec<u32>,
    pub failed: Vec<PageFailure>,
}

fn replace_code(error: &ReplaceError) -> &'static str {
    match error {
        ReplaceError::NoSource(_) => "denoise_page_missing",
        ReplaceError::Cleaned => "denoise_replace_cleaned",
        ReplaceError::SizeChanged { .. } => "denoise_replace_size_changed",
        ReplaceError::Unreadable(_) | ReplaceError::Store(_) => "denoise_replace_unreadable",
    }
}

/// Take every ready page's denoised file as its source, under the job's lock,
/// in one save. Pages with no file to take stay raw and are listed as
/// missing; a file that is there but cannot be used fails the whole replace.
fn replace_pages(job_path: &Path) -> Result<ReplaceReport, String> {
    let _lock = crate::run::lock_job(job_path).map_err(String::from)?;
    let mut job = Job::open(job_path).map_err(|e| e.to_string())?;
    let mut report = ReplaceReport::default();
    for (page_index, source_idx, standing) in standings(&job.project) {
        let path = match standing {
            Standing::Kept => {
                report.kept.push(page_index);
                continue;
            }
            Standing::Missing => {
                report.missing.push(page_index);
                continue;
            }
            Standing::Ready => match job.project.current_denoised(source_idx) {
                Some(row) => row.path.clone(),
                None => {
                    report.missing.push(page_index);
                    continue;
                }
            },
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                report.failed.push(PageFailure { page_index, code: format!("denoise_replace_unreadable: {e}") });
                continue;
            }
        };
        match job.replace_source(source_idx, &bytes) {
            Ok(()) => report.replaced.push(page_index),
            Err(ReplaceError::Store(error)) => return Err(error.to_string()),
            Err(error) => report.failed.push(PageFailure { page_index, code: format!("{}: {error}", replace_code(&error)) }),
        }
    }
    if !report.failed.is_empty() {
        // Nothing is saved, so the chapter is never left half raw and half
        // denoised. Files already copied into the sidecar are named by nothing.
        report.replaced.clear();
        return Ok(report);
    }
    if !report.replaced.is_empty() {
        job.flush().map_err(|e| e.to_string())?;
        crate::library::invalidate_manifest_cache(job_path);
    }
    Ok(report)
}

/// A grey page with seeded noise around mid grey, the size of a real scan.
fn synthetic_page() -> Raster {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let data = (0..BENCH_WIDTH as usize * BENCH_HEIGHT as usize).map(|at| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // Paper, with a band of darker "ink" rows so the model sees edges.
        let base: i32 = if (at / BENCH_WIDTH as usize) % 64 < 8 { 40 } else { 220 };
        (base + (state >> 59) as i32 - 16).clamp(0, 255) as u8
    }).collect();
    Raster {
        width: BENCH_WIDTH, height: BENCH_HEIGHT, mode: ColorMode::Gray, depth: BitDepth::Eight,
        icc: None, palette: None, trns: None, srgb_intent: None, color: Default::default(), data,
    }
}

fn benchmark(app: &tauri::AppHandle, preset_id: &str) -> Result<f64, String> {
    let step = local_preset(preset_id)?;
    let mut engine = open_engine(app, &step)?;
    let page = synthetic_page();
    engine.warm_up().map_err(|e| format!("{}: {e}", e.code()))?;
    let started = std::time::Instant::now();
    engine.denoise(&page, step.strength).map_err(|e| format!("{}: {e}", e.code()))?;
    Ok(started.elapsed().as_secs_f64())
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

/// Denoise `page_indices` (every page when absent) of a chapter on this
/// computer with a local preset, writing `<out_dir>/<source stem>.png`.
/// `run_id` is the interface's name for the run: progress events carry it and
/// [`cancel_denoise_local`] takes it.
#[tauri::command]
pub async fn denoise_chapter_local(
    app: tauri::AppHandle,
    run_id: String,
    chapter_id: String,
    page_indices: Option<Vec<u32>>,
    preset_id: String,
    out_dir: String,
) -> Result<CloudDenoiseReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        run_chapter(&app, &run_id, &chapter_id, page_indices, &preset_id, &PathBuf::from(out_dir))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Stop a denoise run, local or cloud (the name is older than the cloud
/// one's registration). A local run stops at its next tile; a cloud run sends
/// no new page and finishes the ones in flight. Pages already written stay.
/// Answers whether a run by that id was in flight.
#[tauri::command]
pub fn cancel_denoise_local(run_id: String) -> bool {
    let map = runs().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    map.get(&run_id).map(|run| run.cancel.store(true, Ordering::Relaxed)).is_some()
}

/// Seconds one 1284x1809 page takes with this preset on the user's provider,
/// measured after one warm-up tile.
#[tauri::command]
pub async fn benchmark_denoise_local(app: tauri::AppHandle, preset_id: String) -> Result<f64, String> {
    tauri::async_runtime::spawn_blocking(move || benchmark(&app, &preset_id)).await.map_err(|e| e.to_string())?
}

/// Every shipped preset, where it can run, and its recipe.
#[tauri::command]
pub fn denoise_presets() -> Vec<PresetView> {
    presets()
}

/// Take the files the chapter's denoise wrote as its pages, for every page
/// denoised from its bare source and not cleaned since. Detections and their
/// masks stay; the other pages are kept as they are and listed, those with no
/// denoised file to take (never denoised, failed, or gone) as `missing`.
#[tauri::command]
pub async fn replace_with_denoised(app: tauri::AppHandle, chapter_id: String) -> Result<ReplaceReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let job_path = Library::for_app(&app).map_err(|e| e.to_string())?
            .resolve_chapter(&chapter_id).map_err(|e| e.to_string())?;
        replace_pages(&job_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::cloud_denoise::record_written;

    #[test]
    fn presets_list_every_recipe_and_one_local() {
        let listed = presets();
        assert_eq!(listed.len(), 6);
        let value = serde_json::to_value(&listed).unwrap();
        assert_eq!(value[0]["id"], "waifu2x-scan-4x-n2");
        assert_eq!(value[0]["targets"], serde_json::json!(["cloud", "local"]));
        assert_eq!(value[0]["recipe"]["steps"][0]["engine"], "waifu2x-art-scan");
        assert!(value.as_array().unwrap()[1..].iter().all(|preset| preset["targets"] == serde_json::json!(["cloud"])));
        assert!(local_preset("waifu2x-scan-4x-n2").is_ok());
        assert_eq!(local_preset("mangajanai-2x").unwrap_err(), "denoise_preset_not_local");
        assert_eq!(local_preset("sharpest").unwrap_err(), "denoise_preset_unknown");
    }

    #[test]
    fn the_benchmark_page_is_a_grey_scan_sized_page() {
        let page = synthetic_page();
        assert_eq!((page.width, page.height, page.mode, page.depth), (1284, 1809, ColorMode::Gray, BitDepth::Eight));
        assert_eq!(page.data.len(), 1284 * 1809);
        assert_eq!(synthetic_page().data, page.data, "seeded");
        assert!(page.data.iter().any(|v| *v < 60) && page.data.iter().any(|v| *v > 200));
    }

    fn png(mode: ColorMode, width: u32, height: u32) -> Vec<u8> {
        let mut raster = Raster {
            width, height, mode, depth: BitDepth::Eight, icc: None, palette: None, trns: None, srgb_intent: None, color: Default::default(),
            data: Vec::new(),
        };
        if mode == ColorMode::Indexed { raster.palette = Some(vec![0, 0, 0]); }
        raster.data = vec![0; raster.stride() * height as usize];
        encode(&raster, Format::Png).unwrap()
    }

    #[test]
    fn a_local_run_collects_page_failures_and_goes_on() {
        let dir = std::env::temp_dir().join(format!("mc-local-denoise-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pages: Vec<PlannedPage> = (0..5).map(|page_index| PlannedPage {
            page_index, source_idx: page_index as usize, appearance: String::new(), width: 8, height: 8,
        }).collect();
        let names: HashMap<u32, String> = [(0, "001"), (1, "002"), (2, "003"), (3, "004"), (4, "001")]
            .into_iter().map(|(index, name)| (index, name.to_string())).collect();
        let read = |page: &PlannedPage| match page.page_index {
            1 => Err("denoise_page_unreadable: gone".to_string()),
            2 => Ok(png(ColorMode::Indexed, 8, 8)),
            _ => Ok(png(ColorMode::Gray, 8, 8)),
        };
        let mut calls = 0;
        let mut denoise = |raster: &Raster, _: &mut dyn FnMut(usize, usize) -> bool| {
            calls += 1;
            if !raster.mode.allows_model_engines() {
                return Err(engine::Error::Declined(cleaner_core::engines::model::Decline::Mode(raster.mode)));
            }
            Ok(Denoised { raster: raster.clone(), gray: true })
        };
        let mut seen = Vec::new();
        let report = denoise_pages(&pages, &names, &dir, &read, &mut denoise, &AtomicBool::new(false),
            &mut |done, page| seen.push((done, page)));
        assert!(!report.cancelled);
        assert_eq!(seen.iter().filter(|(_, page)| *page == 0.0).map(|(done, _)| *done).max(), Some(5));
        let written: Vec<u32> = report.written.iter().map(|output| output.page_index).collect();
        assert_eq!(written, [0, 3]);
        assert!(dir.join("001.png").is_file() && dir.join("004.png").is_file());
        assert!(report.written.iter().all(|output| output.gray));
        let failed: Vec<(u32, &str)> = report.failed.iter()
            .map(|failure| (failure.page_index, failure.code.split(':').next().unwrap())).collect();
        assert_eq!(failed, [(1, "denoise_page_unreadable"), (2, "denoise_unsupported_page"), (4, "denoise_duplicate_output")]);
        assert_eq!(calls, 3, "an unreadable page and a duplicate name never reach the engine");
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".part")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stopped_run_keeps_what_it_wrote_and_starts_nothing_more() {
        let dir = std::env::temp_dir().join(format!("mc-local-denoise-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pages: Vec<PlannedPage> = (0..4).map(|page_index| PlannedPage {
            page_index, source_idx: page_index as usize, appearance: String::new(), width: 8, height: 8,
        }).collect();
        let names: HashMap<u32, String> = (0..4).map(|index| (index, format!("{:03}", index + 1))).collect();
        let read = |_: &PlannedPage| Ok(png(ColorMode::Gray, 8, 8));
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        // Page 0 is written; page 1 is stopped at its third tile.
        let mut denoise = |raster: &Raster, tick: &mut dyn FnMut(usize, usize) -> bool| {
            calls += 1;
            for done in 0..4 {
                if calls == 2 && done == 2 { cancel.store(true, Ordering::Relaxed); }
                if !tick(done, 4) { return Err(engine::Error::Cancelled); }
            }
            Ok(Denoised { raster: raster.clone(), gray: true })
        };
        let mut last = (0, 0.0);
        let report = denoise_pages(&pages, &names, &dir, &read, &mut denoise, &cancel, &mut |done, page| last = (done, page));
        assert!(report.cancelled);
        assert_eq!(report.written.iter().map(|output| output.page_index).collect::<Vec<_>>(), [0]);
        assert!(report.failed.is_empty(), "a stopped page is not a failure");
        assert_eq!(calls, 2, "no page starts after the stop");
        assert_eq!(last, (1, 0.5));
        assert!(dir.join("001.png").is_file() && !dir.join("002.png").exists());
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["cancelled"], true);
        assert!(serde_json::to_value(CloudDenoiseReport::default()).unwrap().get("cancelled").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replacement_plans_use_visible_dimensions_for_oriented_pages() {
        let root=std::env::temp_dir().join(format!("mc-denoise-orientation-{}",std::process::id()));
        let manifest=a_denoised_chapter(&root);let mut job=Job::open(&manifest).unwrap();
        job.project.sources[0].orientation=cleaner_core::image::orientation::Orientation(6);
        job.project.sources[1].orientation=cleaner_core::image::orientation::Orientation(3);job.flush().unwrap();
        let pages=plan_pages(&manifest,None).unwrap();assert_eq!((pages[0].width,pages[0].height),(6,8));assert_eq!((pages[1].width,pages[1].height),(8,6));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// An 8-bit grey PNG of one value.
    fn grey(width: u32, height: u32, value: u8) -> Vec<u8> {
        let raster = Raster {
            width, height, mode: ColorMode::Gray, depth: BitDepth::Eight, icc: None, palette: None, trns: None,
            srgb_intent: None, color: Default::default(), data: vec![value; width as usize * height as usize],
        };
        encode(&raster, Format::Png).unwrap()
    }

    /// A two-page chapter under `dir`, and a file from a denoise run for each
    /// page, recorded as a run records them: at the appearance each page was
    /// planned at, which is the one it has now.
    fn a_denoised_chapter(dir: &Path) -> PathBuf {
        use cleaner_core::project::StripMode;
        let _ = std::fs::remove_dir_all(dir);
        let raws = dir.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let sources: Vec<_> = (0..2).map(|at| {
            let bytes = grey(8, 6, 200 + at);
            let path = raws.join(format!("{:03}.png", at + 1));
            std::fs::write(&path, &bytes).unwrap();
            cleaner_core::ingest::source_ref(&path, &bytes).unwrap()
        }).collect();
        let manifest = dir.join("job").join("chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(manifest.parent().unwrap(), "test", StripMode::Single, &sources);
        drop(Job::create(&manifest, project).unwrap());
        let pages = plan_pages(&manifest, None).unwrap();
        let out = dir.join("denoised");
        std::fs::create_dir_all(&out).unwrap();
        let mut report = CloudDenoiseReport::default();
        for page in &pages {
            let path = out.join(format!("{:03}.png", page.page_index + 1));
            std::fs::write(&path, grey(8, 6, 40 + page.page_index as u8)).unwrap();
            report.written.push(DenoisedOutput { page_index: page.page_index, path: path.display().to_string(), gray: true });
        }
        record_written(&manifest, &pages, &report, Some("waifu2x-scan-4x-n2"), PresetTarget::Local);
        manifest
    }

    /// A run walking a chapter holds its lock until its last page. Planning a
    /// denoise of it only reads, so it does not wait for that run.
    #[test]
    fn planning_does_not_wait_for_a_run_holding_the_chapter() {
        let dir = std::env::temp_dir().join(format!("mc-denoise-held-{}", std::process::id()));
        let manifest = a_denoised_chapter(&dir);
        let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        let holder = {
            let manifest = manifest.clone();
            std::thread::spawn(move || {
                let _lock = crate::run::lock_job(&manifest).unwrap();
                held.0.send(()).unwrap();
                let _ = release.1.recv();
            })
        };
        held.1.recv().unwrap();
        let (done, answer) = std::sync::mpsc::channel();
        {
            let manifest = manifest.clone();
            std::thread::spawn(move || {
                let pages = plan_pages(&manifest, None).unwrap();
                let _ = done.send(output_names(&manifest, &pages).map(|names| names.len()));
            });
        }
        let planned = answer.recv_timeout(std::time::Duration::from_secs(5));
        release.0.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(planned.expect("planning waited on the held chapter"), Ok(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The offer needs one page to take. A page never denoised, or whose file
    /// is gone, stays raw and is counted apart from a page kept because it
    /// changed, so the confirmation can say which is which.
    #[test]
    fn the_offer_needs_one_page_to_take_and_counts_the_rest() {
        let dir = std::env::temp_dir().join(format!("mc-denoise-offer-{}", std::process::id()));
        let manifest = a_denoised_chapter(&dir);
        let project = Job::open(&manifest).unwrap().project;
        let offer = |pages, kept, missing| Some(DenoiseReplacement { pages, kept, missing });
        assert_eq!(replacement(&project), offer(2, 0, 0));

        let mut changed = project.clone();
        changed.denoised[1].appearance = "before an edit".into();
        assert_eq!(replacement(&changed), offer(1, 1, 0), "a changed page stays");
        let mut gone = project.clone();
        gone.denoised[1].path = dir.join("denoised").join("moved.png");
        assert_eq!(replacement(&gone), offer(1, 0, 1), "a page without its file stays raw");
        let mut partial = project.clone();
        partial.denoised.pop();
        assert_eq!(replacement(&partial), offer(1, 0, 1), "so does a page never denoised");
        changed.denoised[0].appearance = "before an edit".into();
        assert_eq!(replacement(&changed), None, "and with nothing left to take there is no offer");
        let json = serde_json::to_value(replacement(&partial).unwrap()).unwrap();
        assert_eq!(json, serde_json::json!({"pages": 1, "kept": 0, "missing": 1}));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replacing_takes_the_denoised_files_and_keeps_the_detections() {
        use cleaner_core::mask::{Mask, Rect};
        let dir = std::env::temp_dir().join(format!("mc-denoise-replace-{}", std::process::id()));
        let manifest = a_denoised_chapter(&dir);
        let mask = Mask::filled(Rect::new(1, 1, 3, 2));
        {
            let mut job = Job::open(&manifest).unwrap();
            let record = serde_json::from_value(serde_json::json!({
                "id": "c1-p001-d0", "source_idx": 0, "bbox": {"x": 1, "y": 1, "w": 3, "h": 2}, "mask_ref": "",
                "inside": true, "pick": "fill", "detector": "local", "created": 1,
            })).unwrap();
            job.store_detection(record, &mask, &mask).unwrap();
            job.mark_examined(0).unwrap();
            // Page 2 was edited after its denoise.
            job.project.denoised[1].appearance = "before an edit".into();
            job.flush().unwrap();
        }
        let before = Job::open(&manifest).unwrap().project;

        let report = replace_pages(&manifest).unwrap();
        assert_eq!((report.replaced.as_slice(), report.kept.as_slice()), (&[0][..], &[1][..]));
        assert!(report.failed.is_empty());

        let job = Job::open(&manifest).unwrap();
        let denoised = std::fs::read(dir.join("denoised").join("001.png")).unwrap();
        assert_eq!(job.project.sources[0].sha256, cleaner_core::ingest::sha256_hex(&denoised));
        assert_eq!(std::fs::read(job.source_path(0).unwrap()).unwrap(), denoised);
        assert_eq!(job.project.sources[1], before.sources[1], "a kept page keeps its source");
        assert!(job.verify_sources().iter().all(|state| !state.is_stale()));
        assert_eq!(job.load_detection("c1-p001-d0").unwrap().expect("the detection stays").mask, mask);
        assert_eq!(job.project.examined, [0]);
        let rows: Vec<(usize, bool)> = job.project.denoised.iter().map(|row| (row.source_idx, row.taken)).collect();
        assert_eq!(rows, [(0, true), (1, false)], "both rows stay as history, the replaced one taken");
        assert_eq!(job.project.denoised[0].source.as_deref(), Some(before.sources[0].rel_path.as_path()),
            "the taken row keeps the raw page it was made from");
        assert_eq!(replacement(&job.project), None, "spent");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file that is there but cannot be used fails the whole replace; a
    /// page with no file to take stays raw while the others are replaced.
    #[test]
    fn one_bad_file_replaces_nothing_and_a_missing_one_stays_raw() {
        let dir = std::env::temp_dir().join(format!("mc-denoise-replace-bad-{}", std::process::id()));
        let manifest = a_denoised_chapter(&dir);
        std::fs::write(dir.join("denoised").join("002.png"), grey(7, 6, 1)).unwrap();
        let saved = std::fs::read(&manifest).unwrap();
        let report = replace_pages(&manifest).unwrap();
        assert!(report.replaced.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].page_index, 1);
        assert!(report.failed[0].code.starts_with("denoise_replace_size_changed"), "{}", report.failed[0].code);
        assert_eq!(std::fs::read(&manifest).unwrap(), saved, "nothing was saved");

        std::fs::remove_file(dir.join("denoised").join("002.png")).unwrap();
        let raw = Job::open(&manifest).unwrap().project.sources[1].clone();
        let report = replace_pages(&manifest).unwrap();
        assert_eq!((report.replaced.as_slice(), report.missing.as_slice()), (&[0][..], &[1][..]));
        assert!(report.kept.is_empty() && report.failed.is_empty());
        let job = Job::open(&manifest).unwrap();
        assert_eq!(job.project.sources[1], raw, "the page with no file keeps its raw source");
        assert!(job.project.denoised.iter().any(|row| row.source_idx == 0 && row.taken), "page 1 was taken and saved");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_run_id_is_registered_while_it_runs_and_cancel_finds_it() {
        assert!(!cancel_denoise_local("den-test-absent".into()));
        let held = Registered::new("den-test-1", DenoiseKind::Local, "chapter-a").unwrap();
        assert_eq!(Registered::new("den-test-1", DenoiseKind::Cloud, "chapter-b").err().as_deref(),
            Some("denoise_run_id_invalid"));
        assert!(Registered::new("", DenoiseKind::Local, "chapter-a").is_err());
        assert!(cancel_denoise_local("den-test-1".into()));
        assert!(held.cancel().load(Ordering::Relaxed));
        drop(held);
        assert!(!cancel_denoise_local("den-test-1".into()));
    }

    /// Local and cloud runs share the registry: both are listed with their
    /// kind, chapter and progress, and one cancel reaches either.
    #[test]
    fn local_and_cloud_runs_share_the_registry() {
        let local = Registered::new("den-test-share-local", DenoiseKind::Local, "chapter-l").unwrap();
        let cloud = Registered::new("den-test-share-cloud", DenoiseKind::Cloud, "chapter-c").unwrap();
        local.progress(1, 4);
        cloud.progress(9, 3);
        let listed: Vec<_> = jobs().into_iter().filter(|job| job.run_id.starts_with("den-test-share-"))
            .map(|job| (job.run_id, job.kind, job.chapter_id, job.done, job.total)).collect();
        assert_eq!(listed, [
            ("den-test-share-cloud".to_string(), "cloudDenoise", "chapter-c".to_string(), 3, 3),
            ("den-test-share-local".to_string(), "denoise", "chapter-l".to_string(), 1, 4),
        ]);
        assert!(cancel_denoise_local("den-test-share-cloud".into()));
        assert!(cloud.cancel().load(Ordering::Relaxed));
        assert!(!local.cancel().load(Ordering::Relaxed), "a cancel reaches only the run it names");
        drop((local, cloud));
        assert!(jobs().iter().all(|job| !job.run_id.starts_with("den-test-share-")));
        let minted = mint_run_id();
        assert!(minted.starts_with("den-") && minted.len() <= 64 && minted != mint_run_id());
    }
}
