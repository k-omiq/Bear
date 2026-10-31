//! The Runtime screen's two questions: what this machine has for the GPU
//! paths, and which path is fastest for each model here.
//!
//! **This computer.** [`runtime_report`] is [`cleaner_core::cuda`]'s report for
//! the build the screen is choosing, so a user can see before a 455 MB download
//! whether the CUDA 12 build will find a CUDA 12 toolkit and a cuDNN built for
//! it, and which of the two is missing when it will not.
//!
//! **Measurement.** [`measure_backends`] times every installed model on every
//! backend the Settings rows call available for it ([`crate::models`]), with
//! [`cleaner_core::measure`], saves what it found beside the settings, and
//! hands the winners to [`cleaner_core::accel`], where `Automatic` prefers
//! them. A choice the user made per model still wins: only `Automatic` reads
//! a measurement.
//!
//! **A measurement belongs to a setup.** It is saved with
//! [`cleaner_core::measure::setup`] - the runtime library, the devices it
//! found, the NVIDIA driver - and is used only while that is unchanged, so a
//! driver update or a switch to another build puts `Automatic` back on its
//! table until the user measures again. The screen says which it is.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::Manager;

use cleaner_core::accel::{self, Accelerator};
use cleaner_core::cuda;
use cleaner_core::measure;

const FILE: &str = "measurements.json";
const VERSION: u32 = 1;

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Local model work outside any job: a hand clean of one region on a model
/// rung, or the review's analysis. A measurement refuses to start while any
/// is in flight, and none starts while a measurement runs ([`begin_work`]).
static WORK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// SAM's own stop, for the wait on its process-wide permit and a run in flight.
static SAM_CANCEL: std::sync::Mutex<Option<std::sync::Arc<cleaner_core::sam_ts::Cancellation>>> = std::sync::Mutex::new(None);

/// One backend's answer for one model: a timing, or why there is none.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackendResult {
    pub backend: String,
    pub load_ms: Option<f64>,
    pub first_ms: Option<f64>,
    pub median_ms: Option<f64>,
    pub runs: Option<usize>,
    /// An `accel.declined.*` key where the refusal has one, else the
    /// runtime's own words.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelResult {
    /// The Settings row's id: `ctd`, `rtSmall`, `inpainter`, …
    pub id: String,
    pub winner: Option<String>,
    pub results: Vec<BackendResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub version: u32,
    /// Unix milliseconds.
    pub measured_at: u64,
    pub setup: String,
    pub models: Vec<ModelResult>,
}

/// What the screen draws.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementView {
    pub saved: Option<Saved>,
    /// Whether `saved` was taken on the setup in use. `None` while no runtime
    /// is loaded, which this call does not do: loading maps the library, and
    /// Windows will not replace a mapped file, which a runtime download must.
    pub current: Option<bool>,
    pub running: bool,
}

fn app_data(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

fn read_saved(app_data: &Path) -> Option<Saved> {
    let text = std::fs::read_to_string(app_data.join(FILE)).ok()?;
    serde_json::from_str::<Saved>(&text).ok().filter(|saved| saved.version == VERSION)
}

/// `cleaner_core::accel`'s name for each Settings row: the profile the
/// row's sessions open with.
pub(crate) fn profile_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "ctd" => accel::DETECTOR.name,
        "rtSmall" => accel::BALLOON.name,
        "rtFull" => accel::FULL_RT.name,
        "inpainter" => accel::LAMA.name,
        "hayai" => accel::HAYAI.name,
        "samTs" => crate::models::SAM_PROFILE_NAME,
        _ => return None,
    })
}

fn accelerator(id: &str) -> Option<Accelerator> {
    accel::KNOWN.into_iter().find(|accelerator| crate::models::accelerator_id(*accelerator) == id)
}

/// Hand a saved measurement's winners to the selector. It checks the setup
/// itself at every choice, so this is safe before any runtime is loaded.
fn register(saved: Option<&Saved>) {
    accel::set_measured_winners(saved.map(|saved| {
        let winners = saved
            .models
            .iter()
            .filter_map(|model| {
                let winner = accelerator(model.winner.as_deref()?)?;
                Some((profile_name(&model.id)?.to_owned(), winner))
            })
            .collect();
        (saved.setup.clone(), winners)
    }));
}

/// At start: the last measurement, if there is one, is in force from the
/// first session on.
pub fn restore(app: &tauri::AppHandle) {
    if let Ok(dir) = app_data(app) {
        register(read_saved(&dir).as_ref());
    }
}

/// What a model is timed with: its graphs, or its own reader where its graphs
/// cannot be fed synthetic tensors.
enum Subject {
    Graphs { paths: Vec<PathBuf>, dims: &'static [(&'static str, i64)], profile: &'static accel::ModelProfile, threads: Option<usize> },
    Hayai { vision: PathBuf, decoder: PathBuf, tokenizer: PathBuf },
    Sam { dir: PathBuf },
}

/// The first model folder holding every one of `names`.
fn files(app_data: &Path, names: &[&str]) -> Option<Vec<PathBuf>> {
    crate::run::model_search_paths(Some(app_data))
        .into_iter()
        .find(|dir| names.iter().all(|name| dir.join(name).is_file()))
        .map(|dir| names.iter().map(|name| dir.join(name)).collect())
}

/// The dynamic axes take the values a run gives them: one image at a time,
/// and the full detector at its 640-square input (`rt_regions`).
fn subject(id: &str, app_data: &Path) -> Option<Subject> {
    use crate::run::{BALLOONS, DETECTOR, HAYAI_DECODER, HAYAI_TOKENIZER, HAYAI_VISION, INPAINTER};
    let graphs = |names: &[&str], dims, profile, threads| {
        files(app_data, names).map(|paths| Subject::Graphs { paths, dims, profile, threads })
    };
    match id {
        "ctd" => graphs(&[DETECTOR], &[], &accel::DETECTOR, None),
        "rtSmall" => graphs(&[BALLOONS], &[("N", 1)], &accel::BALLOON, None),
        "rtFull" => crate::model_workflows::full_rt_path(app_data).map(|path| Subject::Graphs {
            paths: vec![path],
            dims: &[("N", 1), ("H", 640), ("W", 640)],
            profile: &accel::FULL_RT,
            threads: Some(4),
        }),
        "inpainter" => graphs(&[INPAINTER], &[("batch", 1)], &accel::LAMA, Some(cleaner_core::constants::LAMA_INTRA_THREADS)),
        "hayai" => files(app_data, &[HAYAI_VISION, HAYAI_DECODER, HAYAI_TOKENIZER]).map(|mut paths| {
            let tokenizer = paths.pop().unwrap_or_default();
            let decoder = paths.pop().unwrap_or_default();
            let vision = paths.pop().unwrap_or_default();
            Subject::Hayai { vision, decoder, tokenizer }
        }),
        "samTs" => crate::model_workflows::graph_dir(app_data).map(|dir| Subject::Sam { dir }),
        _ => None,
    }
}

fn time(subject: &Subject, accelerator: Accelerator) -> Result<measure::Timing, String> {
    let cancelled = || CANCEL.load(Ordering::Relaxed);
    match subject {
        Subject::Graphs { paths, dims, profile, threads } => {
            let graphs: Vec<measure::Graph<'_>> = paths.iter().map(|path| measure::Graph { path, dims }).collect();
            measure::time_graphs(&graphs, profile, accelerator, *threads, &cancelled)
        }
        Subject::Hayai { vision, decoder, tokenizer } => {
            measure::time_hayai(vision, decoder, tokenizer, accelerator, &cancelled)
        }
        Subject::Sam { dir } => {
            let cancel = std::sync::Arc::new(cleaner_core::sam_ts::Cancellation::default());
            *SAM_CANCEL.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cancel.clone());
            let timing = measure::time_sam(dir, accelerator, &cancel, &cancelled);
            SAM_CANCEL.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
            timing
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn measure_all(app: &tauri::AppHandle) -> Result<Saved, String> {
    let app_data = app_data(app)?;
    let runtime = cleaner_core::runtime::find(Some(&app_data)).map_err(|e| e.to_string())?;
    cleaner_core::runtime::load(&runtime).map_err(|e| e.to_string())?;
    let setup = measure::setup().ok_or("the runtime did not load")?;

    // Every backend the Settings row calls available, on every installed model.
    let view = crate::models::list_accelerators(app.clone());
    let plan: Vec<(String, Subject, Vec<&'static str>)> = view
        .models
        .iter()
        .filter_map(|row| {
            // One that failed its test run stays in the list, with why.
            let backends: Vec<&'static str> = row
                .backend_status
                .iter()
                .filter(|s| s.supported && (s.available || s.reason_key == Some(accel::SELF_TEST_FAILED)))
                .map(|s| s.id)
                .collect();
            Some((row.id.to_owned(), subject(row.id, &app_data)?, backends))
        })
        .filter(|(_, _, backends)| !backends.is_empty())
        .collect();
    let total: u32 = plan.iter().map(|(_, _, backends)| backends.len() as u32).sum();

    // Nothing parked may hold memory or a GPU while the timings are taken.
    cleaner_core::residency::clear();

    let mut step = 0;
    let mut models = Vec::with_capacity(plan.len());
    for (id, subject, backends) in plan {
        let mut results = Vec::with_capacity(backends.len());
        let mut timings = Vec::new();
        for backend in backends {
            if CANCEL.load(Ordering::Relaxed) {
                return Err("notice.measure.cancelled".to_owned());
            }
            step += 1;
            crate::events::emit(&crate::events::Event::MeasureProgress {
                model: id.clone(),
                backend: backend.to_owned(),
                step,
                total,
                done: false,
            });
            let Some(accelerator) = accelerator(backend) else { continue };
            let outcome = if accelerator.blocked_for(profile_name(&id).unwrap_or_default()) {
                Err(accelerator.proof_failure().unwrap_or_else(|| accel::SELF_TEST_FAILED.to_owned()))
            } else {
                time(&subject, accelerator)
            };
            if CANCEL.load(Ordering::Relaxed) {
                return Err("notice.measure.cancelled".to_owned());
            }
            results.push(match outcome {
                Ok(timing) => {
                    timings.push((accelerator, timing));
                    BackendResult {
                        backend: backend.to_owned(),
                        load_ms: Some(timing.load_ms),
                        first_ms: Some(timing.first_ms),
                        median_ms: Some(timing.median_ms),
                        runs: Some(timing.runs),
                        error: None,
                    }
                }
                Err(error) => BackendResult {
                    backend: backend.to_owned(),
                    load_ms: None,
                    first_ms: None,
                    median_ms: None,
                    runs: None,
                    error: Some(error),
                },
            });
        }
        // The fastest Automatic would actually use, so the winner is one it follows.
        let profile = profile_name(&id);
        timings.retain(|(accelerator, _)| profile.is_none_or(|name| accel::automatic_may_use(name, *accelerator)));
        let winner = measure::fastest(&timings).map(|accelerator| crate::models::accelerator_id(accelerator).to_owned());
        models.push(ModelResult { id, winner, results });
    }
    Ok(Saved { version: VERSION, measured_at: now_ms(), setup, models })
}

fn view_of(saved: Option<Saved>) -> MeasurementView {
    let setup = measure::setup();
    let current = setup.map(|setup| saved.as_ref().is_some_and(|saved| saved.setup == setup));
    MeasurementView { saved, current, running: RUNNING.load(Ordering::Relaxed) }
}

/// The CUDA picture for the build the screen is showing.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReport {
    #[serde(flatten)]
    pub cuda: cuda::Report,
    /// `accel.cudaCheck.*`, for `wanted`.
    pub verdict_key: Option<&'static str>,
    /// What the machine has and what the build needs, for
    /// `accel.cudaCheck.driverTooOld` (driver majors) and `toolkitTooOld`
    /// (CUDA releases).
    pub verdict_have: Option<String>,
    pub verdict_need: Option<String>,
    /// A CUDA build this machine is ready for (`cuda13`, `cuda12`), newest
    /// first. `None` with no NVIDIA adapter or no complete install.
    pub recommended: Option<&'static str>,
    /// The install found is not the one in this process ([`cuda::restart_needed`]).
    pub restart_needed: bool,
}

/// `flavour` is the build the screen is choosing, which may not be the one
/// loaded - before a download nothing is. `rescan` reads the disk and the
/// driver again, for a toolkit installed while the application is open.
///
/// Off the main thread: a cold report runs `nvidia-smi`, which can take a
/// second or more while a laptop's graphics card wakes.
#[tauri::command]
pub async fn runtime_report(flavour: Option<String>, rescan: bool) -> Result<RuntimeReport, String> {
    tauri::async_runtime::spawn_blocking(move || report_for(flavour, rescan)).await.map_err(|e| e.to_string())
}

fn report_for(flavour: Option<String>, rescan: bool) -> RuntimeReport {
    if rescan {
        cuda::rescan();
    }
    let mut report = cuda::report();
    if let Some(major) = flavour.as_deref().and_then(|id| id.strip_prefix("cuda")).and_then(|major| major.parse().ok()) {
        report.wanted = Some(major);
    } else if flavour.is_some() {
        report.wanted = None;
    }
    let recommended = [("cuda13", 13), ("cuda12", 12)].into_iter().find_map(|(id, major)| {
        let candidate = cuda::Report { wanted: Some(major), ..report.clone() };
        matches!(candidate.verdict(), cuda::Verdict::Ready { .. }).then_some(id)
    });
    let verdict = report.wanted.map(|_| report.verdict());
    let (verdict_have, verdict_need) = match &verdict {
        Some(cuda::Verdict::DriverTooOld { have, need }) => (Some(have.to_string()), Some(need.to_string())),
        Some(cuda::Verdict::ToolkitTooOld { have, need }) => (Some(have.clone()), Some(need.clone())),
        _ => (None, None),
    };
    RuntimeReport {
        verdict_key: verdict.as_ref().map(cuda::Verdict::key),
        verdict_have,
        verdict_need,
        recommended,
        restart_needed: cuda::restart_needed(),
        cuda: report,
    }
}

#[tauri::command]
pub async fn read_measurement(app: tauri::AppHandle) -> Result<MeasurementView, String> {
    // Off the main thread for the same reason as `runtime_report`: the
    // fingerprint reads the NVIDIA driver.
    tauri::async_runtime::spawn_blocking(move || Ok(view_of(read_saved(&app_data(&app)?))))
        .await
        .map_err(|e| e.to_string())?
}

/// Time every model on every available backend, keep the fastest, save it.
///
/// Refused while a run is going: the timings would be the run's as much as
/// the model's, and two GPU workloads at once is the memory case the run
/// guards against. Progress is `measure-progress` on the event channel, one
/// per backend timed and a last one with `done`.
#[tauri::command]
pub async fn measure_backends(app: tauri::AppHandle) -> Result<MeasurementView, String> {
    // A run checks `running()` before it starts, and this checks the runs
    // after claiming the flag, so whichever comes second sees the other.
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("notice.measure.running".to_owned());
    }
    if crate::jobs::running() > 0 || WORK.load(Ordering::SeqCst) > 0 {
        settle();
        return Err("notice.measure.busy".to_owned());
    }
    // A download of another build replaced the library this process mapped:
    // it would time the old build and file the result under the new one.
    if cleaner_core::runtime::replaced_since_load() {
        settle();
        return Err("notice.measure.restart".to_owned());
    }
    let worker = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || measure_all(&worker))
        .await
        .map_err(|e| e.to_string())
        .and_then(|outcome| outcome)
        .and_then(|saved| {
            // A Stop pressed during the last run is still a Stop.
            if CANCEL.load(Ordering::SeqCst) {
                return Err("notice.measure.cancelled".to_owned());
            }
            // Nothing could be timed: keep the measurement there was.
            if saved.models.is_empty() {
                return Err("notice.measure.nothing".to_owned());
            }
            save(&app, &saved)?;
            register(Some(&saved));
            Ok(saved)
        });
    // Settled only once the file and the winners are in place, so a screen
    // that reads again on `done` reads this measurement and not the last.
    settle();
    crate::events::emit(&crate::events::Event::MeasureProgress {
        model: String::new(),
        backend: String::new(),
        step: 0,
        total: 0,
        done: true,
    });
    Ok(view_of(Some(outcome?)))
}

fn save(app: &tauri::AppHandle, saved: &Saved) -> Result<(), String> {
    let dir = app_data(app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let partial = dir.join(format!("{FILE}.part"));
    std::fs::write(&partial, serde_json::to_vec_pretty(saved).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, dir.join(FILE)).map_err(|e| e.to_string())
}

/// End a measurement, or a claim refused before it started. `CANCEL` is
/// cleared here rather than at the start, so a Stop pressed while the claim
/// is still being checked is not lost, and after `RUNNING`, so a Stop can
/// never land between the two and stay set for the next measurement.
fn settle() {
    RUNNING.store(false, Ordering::SeqCst);
    CANCEL.store(false, Ordering::SeqCst);
}

/// Held while local model work that is not a job runs; dropping it ends it.
pub(crate) struct Work(());

impl Drop for Work {
    fn drop(&mut self) {
        WORK.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Claim local model work outside a job, or `None` while a measurement runs.
/// Counted before the flag is read, and the measurement reads the count after
/// claiming its flag, so whichever comes second sees the other.
pub(crate) fn begin_work() -> Option<Work> {
    claim_work(&WORK, &RUNNING).then_some(Work(()))
}

fn claim_work(work: &std::sync::atomic::AtomicUsize, running: &AtomicBool) -> bool {
    work.fetch_add(1, Ordering::SeqCst);
    if running.load(Ordering::SeqCst) {
        work.fetch_sub(1, Ordering::SeqCst);
        return false;
    }
    true
}

/// Whether a measurement is running, which a clean waits for.
pub(crate) fn running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// Stop at the next run boundary. `false` when nothing was measuring.
#[tauri::command]
pub fn cancel_measurement() -> bool {
    if !RUNNING.load(Ordering::SeqCst) || CANCEL.swap(true, Ordering::SeqCst) {
        return false;
    }
    if let Some(sam) = SAM_CANCEL.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref() {
        sam.cancel();
    }
    true
}

/// Drop the saved measurement: `Automatic` goes back to the table.
/// Off the main thread like `read_measurement`: the view reads the driver.
#[tauri::command]
pub async fn forget_measurement(app: tauri::AppHandle) -> Result<MeasurementView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = app_data(&app)?.join(FILE);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        register(None);
        Ok(view_of(None))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_is_refused_while_measuring_and_leaves_no_count() {
        let (work, running) = (std::sync::atomic::AtomicUsize::new(0), AtomicBool::new(false));
        assert!(claim_work(&work, &running));
        assert_eq!(work.load(Ordering::SeqCst), 1);
        running.store(true, Ordering::SeqCst);
        assert!(!claim_work(&work, &running));
        assert_eq!(work.load(Ordering::SeqCst), 1);
    }

    fn saved(winner: Option<&str>) -> Saved {
        Saved {
            version: VERSION,
            measured_at: 1,
            setup: "runtime /x".to_owned(),
            models: vec![ModelResult {
                id: "inpainter".to_owned(),
                winner: winner.map(str::to_owned),
                results: vec![BackendResult {
                    backend: "webgpu".to_owned(),
                    load_ms: Some(2169.0),
                    first_ms: Some(646.0),
                    median_ms: Some(639.0),
                    runs: Some(3),
                    error: None,
                }],
            }],
        }
    }

    #[test]
    fn a_saved_measurement_reads_back_and_another_version_does_not() {
        let dir = std::env::temp_dir().join(format!("manga-cleaner-measure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), serde_json::to_vec(&saved(Some("webgpu"))).unwrap()).unwrap();
        assert_eq!(read_saved(&dir), Some(saved(Some("webgpu"))));
        let mut old = serde_json::to_value(saved(None)).unwrap();
        old["version"] = 0.into();
        std::fs::write(dir.join(FILE), old.to_string()).unwrap();
        assert_eq!(read_saved(&dir), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_measured_row_has_a_profile_and_every_backend_id_a_provider() {
        for id in ["ctd", "rtSmall", "rtFull", "samTs", "inpainter", "hayai"] {
            assert!(profile_name(id).is_some(), "{id}");
        }
        for provider in accel::KNOWN {
            assert_eq!(accelerator(crate::models::accelerator_id(provider)), Some(provider));
        }
    }
}
