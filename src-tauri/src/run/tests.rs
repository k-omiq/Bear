//! The ordering contract, pinned rule by rule.
//!
//! Four rules govern the event stream, and a scheduler that satisfies them
//! under a happy path and races under cancellation is not done. So each rule
//! has a test, and the cancel path, the resume path and a chapter whose pages
//! have no regions at all are all exercised against a real [`Job`] on disk.
//!
//! What is *not* exercised here is the model-backed pipeline: opening three
//! ONNX sessions needs the weights, which are not in the repository. That is
//! why [`Cleaner`] is a trait - every rule below is a property of the
//! scheduler, and the scheduler under test is the one the window runs.

use super::*;
use cleaner_core::balloon::BalloonClass;
use cleaner_core::image::{BitDepth, ColorMode, Format, encode, fixtures};
use cleaner_core::project::StripMode;
use cleaner_core::strip::{EdgePad, Strip};

#[test]
fn model_backend_overrides_inherit_and_reject_unknown_values() {
    let settings = serde_json::json!({
        "accelerator": "webgpu",
        "modelAccelerators": {"ctd": "cpu", "rtFull": "cuda", "samTs": "auto"}
    });
    assert_eq!(model_preference_from(&settings, "ctd").unwrap(), Preference::CpuOnly);
    assert_eq!(model_preference_from(&settings, "rtSmall").unwrap(), Preference::Force(accel::Accelerator::WebGpu));
    assert_eq!(model_preference_from(&settings, "rtFull").unwrap(), Preference::Force(accel::Accelerator::Cuda));
    assert_eq!(model_preference_from(&settings, "samTs").unwrap(), Preference::Automatic);
    assert!(model_preference_from(&serde_json::json!({"modelAccelerators": {"ctd": "mlx"}}), "ctd").is_err());
    assert!(model_preference_from(&serde_json::json!({"modelAccelerators": {"ctd": 4}}), "ctd").is_err());
}

#[test]
fn run_slot_is_reserved_before_a_grant_and_released_on_pre_start_error() {
    let _exclusive = exclusively();
    let reservation = reserve_run("chapter-r", &[], RunKind::CloudClean).unwrap().unwrap();
    let busy = reserve_run("chapter-r", &[], RunKind::Clean).unwrap().err().unwrap();
    assert_eq!(busy.already_running, Some(true));
    assert_eq!(busy.at_capacity, None);
    assert_eq!(busy.run_id.as_deref(), Some(reservation.run_id.as_str()));
    drop(reservation);
    let again = reserve_run("chapter-r", &[], RunKind::Clean).unwrap().unwrap();
    drop(again);
    assert!(active().lock().unwrap().is_empty());
}

/// Runs on different chapters each get a slot; a second run on a chapter one
/// of them walks, directly or as part of a project scope, answers
/// `alreadyRunning` with that run's id; past [`MAX_RUNS`] the answer is
/// `atCapacity` with no id. A slot handed back frees its chapter and its
/// share of the cap, and no other.
#[test]
fn runs_on_different_chapters_go_side_by_side_up_to_the_cap() {
    let _exclusive = exclusively();
    let a = reserve_run("chapter-a", &[], RunKind::Detect).unwrap().unwrap();
    let b = reserve_run("chapter-b", &["chapter-b".into(), "chapter-b2".into()], RunKind::Clean).unwrap().unwrap();
    assert_ne!(a.run_id, b.run_id);
    assert!(is_running(&a.run_id) && is_running(&b.run_id));
    assert_eq!(chapter_run("chapter-b2").as_deref(), Some(b.run_id.as_str()), "a project scope holds its other chapters");

    let same = reserve_run("chapter-a", &[], RunKind::Clean).unwrap().err().unwrap();
    assert_eq!((same.run_id.as_deref(), same.already_running, same.at_capacity),
        (Some(a.run_id.as_str()), Some(true), None));
    assert!(same.pages.is_empty());
    let walked = reserve_run("chapter-c", &["chapter-c".into(), "chapter-b2".into()], RunKind::Clean)
        .unwrap().err().unwrap();
    assert_eq!(walked.run_id.as_deref(), Some(b.run_id.as_str()), "a queue over a held chapter");

    let c = reserve_run("chapter-c", &[], RunKind::CloudClean).unwrap().unwrap();
    let full = reserve_run("chapter-d", &[], RunKind::Clean).unwrap().err().unwrap();
    assert_eq!((full.run_id, full.already_running, full.at_capacity), (None, Some(true), Some(true)));
    let wire = serde_json::to_value(reserve_run("chapter-d", &[], RunKind::Clean).unwrap().err().unwrap()).unwrap();
    assert_eq!(wire, serde_json::json!({ "runId": null, "pages": [], "alreadyRunning": true, "atCapacity": true }));
    // Its own chapter still names the run rather than the cap.
    assert_eq!(reserve_run("chapter-c", &[], RunKind::Clean).unwrap().err().unwrap().run_id.as_deref(),
        Some(c.run_id.as_str()));

    drop(b);
    assert!(chapter_run("chapter-b2").is_none());
    assert!(is_running(&a.run_id) && is_running(&c.run_id), "one run's release freed another's slot");
    let d = reserve_run("chapter-d", &[], RunKind::Clean).unwrap().unwrap();
    drop((a, c, d));
    assert!(active().lock().unwrap().is_empty());
}

/// `list_jobs` shows every run in flight with its kind, chapter and progress,
/// and the denoise runs beside them; a finished run leaves it.
#[test]
fn list_jobs_shows_runs_and_denoise_runs_in_flight() {
    let _exclusive = exclusively();
    let detect = reserve_run("chapter-j1", &[], RunKind::of(RunMode::Detect)).unwrap().unwrap();
    detect.set_total(3);
    let clean = reserve_run("chapter-j2", &[], RunKind::of(RunMode::Auto)).unwrap().unwrap();
    clean.set_total(2);
    let cloud = reserve_run("chapter-j3", &[], RunKind::CloudClean).unwrap().unwrap();
    let page = |index: u32| page_done_after_clean(&detect.run_id, "chapter-j1", Path::new("/nonexistent/job"), index);
    detect.progress.observe(&page(0));
    detect.progress.observe(&page_started_event(&detect.run_id, "chapter-j1", 1));
    let denoise = crate::page_denoise::Registered::new(
        "den-test-jobs", crate::page_denoise::DenoiseKind::Cloud, "chapter-j4").unwrap();
    denoise.progress(1, 5);

    let listed: Vec<_> = crate::jobs::snapshot().into_iter()
        .filter(|job| job.chapter_id.starts_with("chapter-j"))
        .map(|job| (job.kind, job.chapter_id, job.done, job.total)).collect();
    assert_eq!(listed, [
        ("detect", "chapter-j1".to_string(), 1, 3),
        ("clean", "chapter-j2".to_string(), 0, 2),
        ("cloudClean", "chapter-j3".to_string(), 0, 0),
        ("cloudDenoise", "chapter-j4".to_string(), 1, 5),
    ]);
    assert_eq!(crate::jobs::list_jobs()[0].run_id, detect.run_id);
    let wire = serde_json::to_value(&crate::jobs::list_jobs()[0]).unwrap();
    assert_eq!(wire, serde_json::json!({
        "runId": detect.run_id, "kind": "detect", "chapterId": "chapter-j1", "done": 1, "total": 3,
    }));
    drop((detect, clean, cloud, denoise));
    assert!(crate::jobs::snapshot().iter().all(|job| !job.chapter_id.starts_with("chapter-j")));
}

/// The quit path raises every run's flag at once and waits for none.
#[test]
fn cancel_all_raises_every_runs_flag() {
    let _exclusive = exclusively();
    let a = reserve_run("chapter-q1", &[], RunKind::Clean).unwrap().unwrap();
    let b = reserve_run("chapter-q2", &[], RunKind::Detect).unwrap().unwrap();
    assert_eq!(cancel_all(), 2);
    assert!(a.cancel.load(Ordering::SeqCst) && b.cancel.load(Ordering::SeqCst));
    drop((a, b));
}

#[test]
fn clean_mode_does_not_preload_onnx_runtime() {
    assert!(!requires_runtime_before_start(RunMode::Clean));
    assert!(requires_runtime_before_start(RunMode::Detect));
    assert!(requires_runtime_before_start(RunMode::Auto));
}

/* ------------------------------------------------------------------ */
/* Scaffolding                                                         */
/* ------------------------------------------------------------------ */

/// A scratch directory that removes itself - the same shape the project store's
/// tests and `library.rs`'s use, and for the same reason: a unique path, not a
/// dependency.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        use std::sync::atomic::AtomicU32;
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir()
            .join("manga-cleaner-run")
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

/// A folder of scans: `count` distinct PNG pages, naturally ordered.
fn scans(dir: &Path, count: usize) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    for n in 1..=count {
        let mut raster = fixtures::by_name("l8").raster;
        // Distinct bytes per page, or ingest deduplicates them.
        raster.data[0] = n as u8;
        let bytes = encode(&raster, Format::Png).unwrap();
        std::fs::write(dir.join(format!("{n:03}.png")), bytes).unwrap();
    }
    dir.to_path_buf()
}

/// A library holding one project of one chapter, over `pages` scans.
fn a_chapter(scratch: &Scratch, pages: usize) -> (Library, String, String) {
    let library = Library::at(scratch.join("library"));
    let source = scans(&scratch.join("raws"), pages);
    let project =
        library.create_project("Tsuki to Hane", StripMode::Single, Some(source), None).unwrap();
    let chapter = library.create_chapter(&project.id, "Ch. 1", None, None).unwrap().created().unwrap();
    (library, project.id, chapter.id)
}

/// A patch covering a small rectangle of the fixture page.
fn a_patch(id: &str, order: u32, engine: Engine) -> Patch {
    let bounds = Rect::new(2, 2, 4, 4);
    let mut pixels = fixtures::by_name("l8").raster;
    pixels.width = bounds.w;
    pixels.height = bounds.h;
    pixels.data = vec![220; (bounds.w * bounds.h) as usize];
    Patch {
        id: id.to_owned(),
        mask: Mask::filled(bounds),
        ink: Mask::filled(bounds),
        pixels,
        order,
        visible: true,
        provenance: Provenance {
            engine,
            engine_version: "test".into(),
            model_sha256: None,
            execution_provider: "cpu".into(),
            params_snapshot: serde_json::json!({
                "rung": engine.rung_key(),
                "fill_mode": "match-surround",
                "elapsed_ms": 41,
            }),
            mask_sha256: String::new(),
            source_sha256: "0".repeat(64),
            cloud: None,
            created: 1_760_000_000,
        },
    }
}

/// A cleaner that produces a fixed shape of result, and records what it was
/// asked for.
///
/// Everything the scheduler is responsible for - the order of events, the
/// manifest writes, the cancellation checks - is visible through this without
/// a single ONNX session.
struct Stub {
    cleaned: usize,
    untouched: usize,
    /// Regions declined rather than gate-skipped, which is the other counter.
    declined: usize,
    pages: Arc<Mutex<Vec<String>>>,
    ceilings: Arc<Mutex<Vec<Engine>>>,
    /// What the strip said about each page it was asked for: the placement, how
    /// many detection segments the page has, and how many joins the chapter
    /// has.
    contexts: Arc<Mutex<Vec<(usize, usize, usize)>>>,
    /// Pages whose clean fails outright, by page id.
    fails: Vec<String>,
    /// Pages whose clean **panics**, by page id. A model that produced a shape
    /// nothing expected is this, and it is not a `Result`.
    panics: Vec<String>,
    /// Pages whose clean fails the way a withdrawn cloud authority does: the
    /// failure ends the run. By page id.
    stops: Vec<String>,
    stopped: Option<String>,
    /// Report the held-back regions before the cleaned ones. The order a page's
    /// regions come back in is the pipeline's business and not the scheduler's,
    /// and it decides what a cancel mid-page leaves written down.
    untouched_first: bool,
}

impl Stub {
    fn new(cleaned: usize, untouched: usize) -> Stub {
        Stub {
            cleaned,
            untouched,
            declined: 0,
            pages: Arc::new(Mutex::new(Vec::new())),
            ceilings: Arc::new(Mutex::new(Vec::new())),
            contexts: Arc::new(Mutex::new(Vec::new())),
            fails: Vec::new(),
            panics: Vec::new(),
            stops: Vec::new(),
            stopped: None,
            untouched_first: false,
        }
    }

    /// A page whose held-back regions are written down before its cleaned ones,
    /// with one declined region among them.
    fn held_back_first(cleaned: usize, untouched: usize, declined: usize) -> Stub {
        Stub { declined, untouched_first: true, ..Stub::new(cleaned, untouched) }
    }

    fn held_back(&self) -> Vec<RegionOutcome> {
        let mut regions = Vec::new();
        for index in 0..self.untouched {
            regions.push(RegionOutcome::Untouched {
                bbox: Rect::new(20 + index as i64, 20, 4, 4),
                reason: "review.reason.gateSkippedNotJapanese".into(),
            });
        }
        for index in 0..self.declined {
            regions.push(RegionOutcome::Declined {
                bbox: Rect::new(40 + index as i64, 40, 4, 4),
                reason: "decline.reason.histogram".into(),
                lettering: Some((Mask::filled(Rect::new(41 + index as i64, 41, 2, 2)), 0.5)),
            });
        }
        regions
    }
}

impl Cleaner for Stub {
    fn clean_page(
        &mut self,
        page_id: &str,
        _bytes: &[u8],
        ceiling: Engine,
        context: &PageContext,
    ) -> Result<PageOutcome, String> {
        self.pages.lock().unwrap().push(page_id.to_owned());
        self.ceilings.lock().unwrap().push(ceiling);
        // The geometry is handed to every page and every page is somewhere in
        // the strip. Asserted here rather than in one test, so a scheduler that
        // stopped building the context would fail every test that runs a page
        // rather than the one that looks for it.
        assert!(
            context.strip.pages().get(context.placement).is_some(),
            "{page_id} was cleaned with no place in the strip"
        );
        assert!(
            !context.page_segments().is_empty(),
            "{page_id} was cleaned with no detection segment"
        );
        self.contexts
            .lock()
            .unwrap()
            .push((context.placement, context.page_segments().len(), context.joins.len()));
        assert!(!self.panics.iter().any(|id| id == page_id), "the pipeline panicked on {page_id}");
        if self.fails.iter().any(|id| id == page_id) {
            return Err("this page could not be read".into());
        }
        if self.stops.iter().any(|id| id == page_id) {
            self.stopped = Some("cloud_run_profile_changed".into());
            return Err("cloud_analysis_failed: cloud_run_profile_changed".into());
        }
        let mut outcome = PageOutcome::default();
        if self.untouched_first {
            outcome.regions.extend(self.held_back());
        }
        for index in 0..self.cleaned {
            outcome.regions.push(RegionOutcome::Cleaned(
                Box::new(a_patch(&region_id(page_id, index), index as u32, Engine::Fill)),
                None,
            ));
        }
        if !self.untouched_first {
            outcome.regions.extend(self.held_back());
        }
        Ok(outcome)
    }

    fn stop_reason(&self) -> Option<String> {
        self.stopped.clone()
    }
}

/// The scheduler's globals - the active slot and the notice counter - are
/// process-wide, and `cargo test` is parallel. The tests that touch them take
/// this first, the same shape `events.rs`'s own tests use and for the same
/// reason.
fn exclusively() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run a body with panic reporting silenced, for the tests whose subject *is* a
/// panic. The hook goes back immediately after, so a genuine panic elsewhere
/// still says so.
fn quietly<T>(body: impl FnOnce() -> T) -> T {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let answer = body();
    std::panic::set_hook(hook);
    answer
}

/// Whether a job is free to be written right now: no thread of this process
/// holds it.
fn job_is_free(path: &Path) -> bool {
    !cleaner_core::project::lock::held_in_process(path)
}

/// The worker, and the active slot it will hand back, wired to one recorder.
fn a_worker(chapter_id: &str, recorder: &Recorder) -> Worker {
    let worker = Worker {
        run_id: "backend-run-1".to_owned(),
        chapter_id: chapter_id.to_owned(),
        mode: RunMode::Auto,
        cancel: Arc::clone(&recorder.cancel),
        finished: Arc::new((Mutex::new(false), Condvar::new())),
        outcome: Arc::new(Mutex::new(None)),
    };
    active().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(Active {
        run_id: worker.run_id.clone(),
        chapter_id: chapter_id.to_owned(),
        chapters: vec![chapter_id.to_owned()],
        kind: RunKind::Clean,
        progress: Arc::new(RunProgress::default()),
        cancel: Arc::clone(&worker.cancel),
        finished: Arc::clone(&worker.finished),
        outcome: Arc::clone(&worker.outcome),
    });
    worker
}

/// Collects the stream, and can pull the cancel lever at a chosen moment -
/// which is how a cancel arriving *while* the run is between two things is
/// simulated without a second thread and the flakiness that comes with one.
struct Recorder {
    seen: Arc<Mutex<Vec<Event>>>,
    cancel: Arc<AtomicBool>,
    /// Raise the cancel flag the first time an event of this kind goes out.
    trip_on: Option<&'static str>,
}

impl Recorder {
    fn new() -> Recorder {
        Recorder {
            seen: Arc::new(Mutex::new(Vec::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            trip_on: None,
        }
    }

    fn tripping_on(kind: &'static str) -> Recorder {
        Recorder { trip_on: Some(kind), ..Recorder::new() }
    }

    fn sink(&self) -> impl Fn(Event) + '_ {
        move |event| {
            if let Some(kind) = self.trip_on {
                if kind_of(&event) == kind && !self.cancel.load(Ordering::SeqCst) {
                    self.cancel.store(true, Ordering::SeqCst);
                }
            }
            self.seen.lock().unwrap().push(event);
        }
    }

    fn events(&self) -> Vec<Event> {
        self.seen.lock().unwrap().clone()
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.events().iter().map(kind_of).collect()
    }
}

fn kind_of(event: &Event) -> &'static str {
    match event {
        Event::PageStarted { .. } => "page-started",
        Event::RegionDone { .. } => "region-done",
        Event::PageDone { .. } => "page-done",
        Event::RunFinished { .. } => "run-finished",
        Event::Notice { .. } => "notice",
        // A download's progress. Never emitted by a run, and named here rather
        // than swept into a wildcard so that the next event type added to the
        // seam fails this match instead of being silently mislabelled.
        Event::ModelProgress { .. } => "model-progress",
    }
}

fn page_index_of(event: &Event) -> Option<u32> {
    match event {
        Event::PageStarted { page_index, .. }
        | Event::RegionDone { page_index, .. }
        | Event::PageDone { page_index, .. } => Some(*page_index),
        _ => None,
    }
}

/* ------------------------------------------------------------------ */
/* The four ordering rules                                             */
/* ------------------------------------------------------------------ */

/// Rule 1: every `region-done` for a page falls **strictly** between that
/// page's `page-started` and its `page-done`.
#[test]
fn every_region_done_falls_strictly_between_its_pages_start_and_end() {
    let scratch = Scratch::new("ordering");
    let (library, _, chapter_id) = a_chapter(&scratch, 4);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    assert_eq!(entries.len(), 4);

    let recorder = Recorder::new();
    let mut stub = Stub::new(3, 1);
    let summary = execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut stub,
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    assert_eq!(summary.reason, "completed");
    let events = recorder.events();
    for page in 0..4u32 {
        let opened = events
            .iter()
            .position(|e| kind_of(e) == "page-started" && page_index_of(e) == Some(page))
            .expect("a page was never started");
        let closed = events
            .iter()
            .position(|e| kind_of(e) == "page-done" && page_index_of(e) == Some(page))
            .expect("a page was never finished");
        assert!(opened < closed);
        let regions: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| kind_of(e) == "region-done" && page_index_of(e) == Some(page))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(regions.len(), 3, "a page's regions were not all reported");
        for at in regions {
            assert!(at > opened && at < closed, "a region escaped its page's bracket");
        }
    }
    // The pages themselves are in queue order, and nothing interleaves them.
    let page_order: Vec<u32> = events
        .iter()
        .filter(|e| kind_of(e) == "page-started")
        .filter_map(page_index_of)
        .collect();
    assert_eq!(page_order, vec![0, 1, 2, 3]);
}

/// Rule 2: exactly one `run-finished` per run, whatever happened. Asserted on
/// both exits, because one of them is the one a race would double.
#[test]
fn exactly_one_run_finished_whatever_happened() {
    for trip in [None, Some("region-done"), Some("page-done")] {
        let scratch = Scratch::new("one-finish");
        let (library, _, chapter_id) = a_chapter(&scratch, 4);
        let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
        let recorder = match trip {
            None => Recorder::new(),
            Some(kind) => Recorder::tripping_on(kind),
        };
        let mut stub = Stub::new(2, 0);
        execute(
            "run-1",
            &chapter_id,
            &entries,
            &mut stub,
            Engine::Flux,
            &recorder.cancel,
            &recorder.sink(),
        );
        let finished = recorder.kinds().iter().filter(|kind| **kind == "run-finished").count();
        assert_eq!(finished, 1, "trip {trip:?} produced {finished} run-finished events");
        assert_eq!(
            recorder.kinds().last(),
            Some(&"run-finished"),
            "something was emitted after the run finished"
        );
    }
}

/// Rule 3: `nextPageIndex` is set **only** on cancel, and it is where a resume
/// picks up - the page the run stopped at, not the last one it finished.
#[test]
fn next_page_index_is_set_only_on_cancel() {
    let scratch = Scratch::new("next-page");
    let (library, _, chapter_id) = a_chapter(&scratch, 4);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();

    let completed = Recorder::new();
    let summary = execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(1, 0),
        Engine::Flux,
        &completed.cancel,
        &completed.sink(),
    );
    assert_eq!(summary.reason, "completed");
    assert_eq!(summary.next_page_index, None);
    assert!(matches!(
        completed.events().last(),
        Some(Event::RunFinished { next_page_index: None, .. })
    ));

    // A second chapter, because the first one is now finished and a finished
    // chapter has an empty queue.
    let other = Scratch::new("next-page-cancel");
    let (library, _, chapter_id) = a_chapter(&other, 4);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let cancelled = Recorder::tripping_on("page-done");
    let summary = execute(
        "run-2",
        &chapter_id,
        &entries,
        &mut Stub::new(1, 0),
        Engine::Flux,
        &cancelled.cancel,
        &cancelled.sink(),
    );
    assert_eq!(summary.reason, "cancelled");
    // Page 0 finished; the cancel landed as it did, so page 1 is the resume
    // point and page 1 is what the manifest records.
    assert_eq!(summary.next_page_index, Some(1));
    assert_eq!(summary.pages_cleaned, 1);
    let job = Job::open(&entries[0].job_path).unwrap();
    assert_eq!(job.project.interrupted_at, Some(1));
}

/// Rule 4 lives in `events.rs` - `page-started` is a fifth event *type*, not a
/// flag - and here is the half a scheduler can get wrong: a page that is being
/// cleaned announces itself before anything about it is known, so the mark can
/// go up while the work happens rather than after it.
#[test]
fn a_page_announces_itself_before_any_of_its_regions() {
    let scratch = Scratch::new("announce");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let recorder = Recorder::new();
    execute(
        "run-1",
        &chapter_id,
        &plan(&library, "chapter", &chapter_id, None).unwrap(),
        &mut Stub::new(2, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );
    assert_eq!(
        recorder.kinds(),
        vec!["page-started", "region-done", "region-done", "page-done", "run-finished"]
    );
}

/* ------------------------------------------------------------------ */
/* Cancel                                                              */
/* ------------------------------------------------------------------ */

/// A cancel keeps every completed region, and the page it landed in the
/// middle of is left unfinished rather than rolled back - no `page-done`,
/// not marked examined, and still queued.
#[test]
fn a_cancel_mid_page_keeps_what_finished_and_leaves_the_page_queued() {
    let scratch = Scratch::new("cancel-mid-page");
    let (library, _, chapter_id) = a_chapter(&scratch, 3);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();

    // Trips on the first `region-done`, which is inside page 0.
    let recorder = Recorder::tripping_on("region-done");
    let summary = execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(3, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    assert_eq!(summary.reason, "cancelled");
    assert_eq!(summary.next_page_index, Some(0));
    assert_eq!(summary.pages_cleaned, 0);
    assert_eq!(summary.regions_cleaned, 1);
    assert!(
        !recorder.kinds().contains(&"page-done"),
        "a page cancelled part-way was reported finished"
    );

    let job = Job::open(&entries[0].job_path).unwrap();
    assert_eq!(job.project.patches.len(), 1, "the completed region was not kept");
    assert!(job.project.examined.is_empty(), "an unfinished page was marked examined");
    assert_eq!(job.project.interrupted_at, Some(0));

    // And the next run picks the page up again.
    let again = plan(&library, "chapter", &chapter_id, None).unwrap();
    assert_eq!(again.iter().map(|e| e.page_index).collect::<Vec<_>>(), vec![0, 1, 2]);
}

/// Nothing is emitted after the cancel is observed. A scheduler that kept
/// walking would report regions for a run the interface has already closed.
#[test]
fn a_cancelled_run_stops_emitting() {
    let scratch = Scratch::new("cancel-stops");
    let (library, _, chapter_id) = a_chapter(&scratch, 6);
    let recorder = Recorder::tripping_on("page-done");
    execute(
        "run-1",
        &chapter_id,
        &plan(&library, "chapter", &chapter_id, None).unwrap(),
        &mut Stub::new(1, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );
    assert_eq!(
        recorder.kinds(),
        vec!["page-started", "region-done", "page-done", "run-finished"],
        "a cancelled run kept walking its queue"
    );
}

/* ------------------------------------------------------------------ */
/* Resume                                                              */
/* ------------------------------------------------------------------ */

/// A resumed run picks up at the page the cancel stopped at, redoes it, and
/// clears the offer when it finishes - a job that records being interrupted
/// and cannot say it no longer is would offer a resume for ever.
#[test]
fn a_resume_starts_at_the_page_the_cancel_stopped_at_and_clears_the_offer() {
    let scratch = Scratch::new("resume");
    let (library, project_id, chapter_id) = a_chapter(&scratch, 4);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();

    let first = Recorder::tripping_on("page-done");
    let summary = execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(1, 0),
        Engine::Flux,
        &first.cancel,
        &first.sink(),
    );
    assert_eq!(summary.next_page_index, Some(1));

    // What Home reads to offer the resume.
    let project = library
        .list_projects()
        .unwrap()
        .into_iter()
        .find(|p| p.id == project_id)
        .unwrap();
    let offer = project.interrupted_job.expect("no resume was offered");
    assert_eq!(offer.chapter_id, chapter_id);
    assert_eq!(offer.page_index, 1);

    let resumed = plan(&library, "chapter", &chapter_id, None).unwrap();
    assert_eq!(
        resumed.iter().map(|entry| entry.page_index).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the resume did not pick up where the cancel stopped"
    );

    let second = Recorder::new();
    let summary = execute(
        "run-2",
        &chapter_id,
        &resumed,
        &mut Stub::new(1, 0),
        Engine::Flux,
        &second.cancel,
        &second.sink(),
    );
    assert_eq!(summary.reason, "completed");
    assert_eq!(summary.pages_cleaned, 3);
    assert_eq!(Job::open(&entries[0].job_path).unwrap().project.interrupted_at, None);
    assert!(
        library
            .list_projects()
            .unwrap()
            .into_iter()
            .find(|p| p.id == project_id)
            .unwrap()
            .interrupted_job
            .is_none(),
        "the resume was still offered after the run completed"
    );
}

/* ------------------------------------------------------------------ */
/* A chapter with nothing in it                                        */
/* ------------------------------------------------------------------ */

/// A chapter whose pages have no regions at all. Every page is still started
/// and finished, no `region-done` goes out, and the run reports the empty
/// result - which is also the only thing that can tell "detected nothing"
/// from "never looked".
#[test]
fn a_chapter_with_no_regions_at_all_reports_every_page_and_no_regions() {
    let scratch = Scratch::new("empty");
    let (library, _, chapter_id) = a_chapter(&scratch, 5);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();

    let recorder = Recorder::new();
    let summary = execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(0, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    assert_eq!(summary.reason, "completed");
    assert_eq!(summary.pages_cleaned, 5);
    assert_eq!(summary.regions_cleaned, 0);
    let kinds = recorder.kinds();
    assert_eq!(kinds.iter().filter(|k| **k == "region-done").count(), 0);
    assert_eq!(kinds.iter().filter(|k| **k == "page-started").count(), 5);
    assert_eq!(kinds.iter().filter(|k| **k == "page-done").count(), 5);
    assert_eq!(kinds.iter().filter(|k| **k == "run-finished").count(), 1);

    // The chapter now says so at the seam, and its pages are finished rather
    // than permanently unclean.
    let chapter = library
        .list_projects()
        .unwrap()
        .into_iter()
        .flat_map(|project| project.chapters)
        .find(|chapter| chapter.id == chapter_id)
        .unwrap();
    assert!(chapter.no_text_detected, "an examined and empty chapter did not say so");
    assert!(chapter.pages.iter().all(|page| page.status == "cleaned"));

    // And nothing is queued a second time.
    assert!(plan(&library, "chapter", &chapter_id, None).unwrap().is_empty());
}

/// The complementary case: a chapter nobody has run does **not** claim that
/// no text was detected. The two states were the same manifest before
/// `Project::examined` existed.
#[test]
fn a_chapter_nobody_has_run_does_not_claim_an_empty_result() {
    let scratch = Scratch::new("unexamined");
    let (library, _, chapter_id) = a_chapter(&scratch, 3);
    let chapter = library
        .list_projects()
        .unwrap()
        .into_iter()
        .flat_map(|project| project.chapters)
        .find(|chapter| chapter.id == chapter_id)
        .unwrap();
    assert!(!chapter.no_text_detected);
    assert!(chapter.pages.iter().all(|page| page.status == "unclean"));
}

/// A cancelled run has not established that a chapter is empty, however many
/// of its pages came back with nothing.
#[test]
fn a_partly_examined_chapter_does_not_claim_an_empty_result() {
    let scratch = Scratch::new("part-examined");
    let (library, _, chapter_id) = a_chapter(&scratch, 4);
    let recorder = Recorder::tripping_on("page-done");
    execute(
        "run-1",
        &chapter_id,
        &plan(&library, "chapter", &chapter_id, None).unwrap(),
        &mut Stub::new(0, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );
    let chapter = library
        .list_projects()
        .unwrap()
        .into_iter()
        .flat_map(|project| project.chapters)
        .find(|chapter| chapter.id == chapter_id)
        .unwrap();
    assert!(!chapter.no_text_detected, "one page of four settled the whole chapter");
}

/* ------------------------------------------------------------------ */
/* The queue                                                           */
/* ------------------------------------------------------------------ */

#[test]
fn scope_page_queues_one_page_and_scope_chapter_queues_them_all() {
    let scratch = Scratch::new("scope");
    let (library, _, chapter_id) = a_chapter(&scratch, 6);
    let one = plan(&library, "page", &chapter_id, Some(3)).unwrap();
    assert_eq!(one.iter().map(|entry| entry.page_index).collect::<Vec<_>>(), vec![3]);
    assert_eq!(plan(&library, "chapter", &chapter_id, None).unwrap().len(), 6);
}

#[test]
fn scope_project_walks_every_chapter_of_the_project() {
    let scratch = Scratch::new("scope-project");
    let (library, project_id, first) = a_chapter(&scratch, 2);
    scans(&scratch.join("raws/Ch. 2"), 3);
    let second = library.create_chapter(&project_id, "Ch. 2", None, None).unwrap().created().unwrap();

    let entries = plan(&library, "project", &first, None).unwrap();
    let chapters: Vec<&str> = entries.iter().map(|entry| entry.chapter_id.as_str()).collect();
    assert_eq!(chapters.iter().filter(|id| **id == second.id).count(), 3);
    assert_eq!(chapters.iter().filter(|id| **id == first).count(), 2);
}

/// A page with a patch on it is finished. A run over a chapter that is already
/// half done queues the other half and nothing else.
#[test]
fn a_page_that_has_already_been_cleaned_is_not_queued_again() {
    let scratch = Scratch::new("requeue");
    let (library, _, chapter_id) = a_chapter(&scratch, 3);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let recorder = Recorder::new();
    execute(
        "run-1",
        &chapter_id,
        &entries[..1],
        &mut Stub::new(2, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );
    assert_eq!(
        plan(&library, "chapter", &chapter_id, None)
            .unwrap()
            .iter()
            .map(|entry| entry.page_index)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

/// A chapter of an unknown id is an error rather than an empty queue: an empty
/// queue is `notice.run.nothingInScope`, which is a different thing to tell a
/// user.
#[test]
fn an_unknown_chapter_is_an_error_rather_than_an_empty_queue() {
    let scratch = Scratch::new("unknown");
    let (library, _, _) = a_chapter(&scratch, 1);
    assert!(matches!(
        plan(&library, "chapter", "nope", None),
        Err(LibraryError::Unknown { .. })
    ));
}

/* ------------------------------------------------------------------ */
/* The ceiling                                                         */
/* ------------------------------------------------------------------ */

/// **Auto clean is local-only.** A batch run can never reach the cloud rung,
/// whatever it is asked for - `needs-confirmation` lives on `applyTool` and
/// `runClean` has no equivalent, so a run at the cloud ceiling would spend with
/// neither the transmission statement nor the cost confirmation.
#[test]
fn a_run_can_never_reach_the_cloud_rung() {
    for asked in ["cloud", "ladder.rung.cloud"] {
        assert_eq!(effective_ceiling(Some(asked), None), Engine::Flux);
        assert_eq!(effective_ceiling(None, Some(asked)), Engine::Flux);
        assert_eq!(effective_ceiling(Some(asked), Some(asked)), Engine::Flux);
    }
}

/// **`engineCeiling` caps, never raises.** Introduced twice, caught twice.
#[test]
fn the_ceiling_only_ever_moves_down() {
    // The caller lowers the stored setting.
    assert_eq!(effective_ceiling(Some("fill"), Some("flux")), Engine::Fill);
    // And cannot lift it.
    assert_eq!(effective_ceiling(Some("flux"), Some("fill")), Engine::Fill);
    // Neither side named one: the highest local rung, not the highest rung.
    assert_eq!(effective_ceiling(None, None), HIGHEST_LOCAL);
    // A name this build does not know is ignored rather than read as "no
    // ceiling" - the failure that matters is a typo unlocking a rung.
    assert_eq!(effective_ceiling(Some("supermax"), Some("lama")), Engine::Lama);
    assert_eq!(effective_ceiling(Some("supermax"), None), HIGHEST_LOCAL);
}

/// A stored `denoise` ceiling, from before rung 1 was removed, is the fill: it
/// allowed no model then and allows none now.
#[test]
fn a_stored_denoise_ceiling_reads_as_the_fill() {
    assert_eq!(parse_rung("denoise"), Some(Engine::Fill));
    assert_eq!(effective_ceiling(Some("denoise"), None), Engine::Fill);
    assert_eq!(engine_for(Route::Fill, Engine::Fill), Some(Engine::Fill));
}

/// The ladder exists now, and an inpaint route reaches it - at every ceiling
/// that permits rung 2, and at none that does not.
///
/// An `Inpaint` region under a ceiling of rung 0 would get a hole where the
/// screentone was, and §6's metric would not catch it - a flat plate has no
/// strokes to fail the edge test and its level sits inside a halftone's own
/// p5..p95.
#[test]
fn an_inpaint_route_reaches_rung_two_and_declines_below_it() {
    for ceiling in [Engine::Lama, Engine::Flux] {
        assert_eq!(engine_for(Route::Inpaint, ceiling), Some(Engine::Lama));
    }
    assert_eq!(engine_for(Route::Inpaint, Engine::Fill), None);
}

/// **The picks move where a region starts and nothing else.** Bubble text
/// starts on the fill even where the fit sent it to the inpainter -
/// which is the whole point of the row, a balloon being flat paper - and text
/// outside a balloon starts on the inpainter even where the fit would have
/// filled it.
#[test]
fn a_pick_moves_the_starting_rung_for_its_kind_of_text() {
    let picks = Picks::default();
    // The default the tool window ships: a bubble is filled, whatever the fit
    // made of the paper around the text.
    assert_eq!(
        start_rung(Route::Inpaint, Engine::Flux, Some(picks.bubble)),
        Some(Engine::Fill)
    );
    // Outside a balloon the art has to be put back, so the inpainter is where
    // it starts rather than where it escalates to.
    assert_eq!(
        start_rung(Route::Fill, Engine::Flux, Some(picks.outside)),
        Some(Engine::Lama)
    );
    // No pick is the route's own answer, unchanged.
    assert_eq!(start_rung(Route::Fill, Engine::Flux, None), Some(Engine::Fill));
    assert_eq!(start_rung(Route::Inpaint, Engine::Flux, None), Some(Engine::Lama));
}

/// **A pick is not a ceiling, in either direction.** It cannot reach past
/// `engineCeiling`, and it cannot talk an inpaint route down past the decline
/// [`engine_for`] states - §5's argument is about the paper under the region,
/// and a preference does not change what is under it.
#[test]
fn a_pick_can_neither_lift_the_ceiling_nor_undo_a_decline() {
    // A rung-2 pick under a ceiling that cannot inpaint keeps the routed rung.
    assert_eq!(
        start_rung(Route::Fill, Engine::Fill, Some(EnginePick::Lama)),
        Some(Engine::Fill)
    );
    // And an inpaint route under one still declines rather than being filled.
    assert_eq!(start_rung(Route::Inpaint, Engine::Fill, Some(EnginePick::Fill)), None);
    assert_eq!(start_rung(Route::Inpaint, Engine::Fill, Some(EnginePick::Lama)), None);
}

/// The seam's two names, and what an absent or mistyped one means: **this
/// type's own default**, not "no preference". The interface always sends both,
/// so an unreadable one is a typo and the defaults are what the window shows.
#[test]
fn the_picks_read_the_seams_names_and_default_rather_than_falling_open() {
    assert_eq!(
        Picks::from_args(Some("lama"), Some("fill")),
        Picks { bubble: EnginePick::Lama, outside: EnginePick::Fill }
    );
    assert_eq!(Picks::from_args(None, None), Picks::default());
    assert_eq!(Picks::from_args(Some("supermax"), Some("")), Picks::default());
    // Catalogue keys are accepted too, and so is `redraw` - the word the rows
    // used to send, kept readable so a stored preference is not silently
    // replaced by the default.
    // `denoise` is a removed rung; a stored one starts on the fill.
    assert_eq!(
        Picks::from_args(Some("ladder.rung.lama"), Some("denoise")),
        Picks { bubble: EnginePick::Lama, outside: EnginePick::Fill }
    );
    assert_eq!(
        Picks::from_args(Some("redraw"), Some("inpaint")),
        Picks { bubble: EnginePick::Lama, outside: EnginePick::Lama }
    );
    // Neither rung above `HIGHEST_AUTOMATIC` is a pick: a run may not reach
    // them, so reading one as a start would be a promise the ceiling takes
    // straight back.
    assert_eq!(Picks::from_args(Some("flux"), Some("cloud")), Picks::default());
}

/// The escalation loop's list: at or above the routed rung, at or below the
/// ceiling, and only rungs that can carry this source.
#[test]
fn the_ladder_climbs_from_the_routed_rung_and_never_above_the_ceiling() {
    let page = fixtures::by_name("l8").raster;
    assert_eq!(
        ladder_from(Engine::Fill, Engine::Flux, &page),
        vec![Engine::Fill, Engine::Lama]
    );
    // A region that started at rung 2 never falls back to a simpler engine:
    // §5's table is a floor and not a suggestion - and rung 2 is now the top of
    // the list, so this is also where the escalation ends.
    assert_eq!(ladder_from(Engine::Lama, Engine::Flux, &page), vec![Engine::Lama]);
    assert_eq!(ladder_from(Engine::Fill, Engine::Fill, &page), vec![Engine::Fill]);

    // Rung 3a is never on any list, at any ceiling.
    assert!(!ladder_from(Engine::Fill, Engine::Flux, &page).contains(&Engine::Flux));

    // And a source a rung cannot carry is not a rung the escalation names -
    // enforced one level up from the router.
    let bitonal = fixtures::by_name("bitonal").raster;
    assert_eq!(ladder_from(Engine::Fill, Engine::Flux, &bitonal), vec![Engine::Fill]);
    let indexed = fixtures::by_name("indexed-p").raster;
    assert_eq!(ladder_from(Engine::Fill, Engine::Flux, &indexed), vec![Engine::Fill]);
}

/// **Nothing above rung 2 is reachable from an automatic run**, at any ceiling,
/// from any starting rung, on any source.
///
/// This is the same ruling the cloud rung carries: `runClean` has no
/// confirmation protocol, and a batch job must not silently spend ten to
/// sixty seconds a region on a heavy local model, nor spawn a multi-gigabyte
/// child to do it. The rung is reached per region, deliberately, from
/// Content-aware fill.
///
/// This existed as a comment on [`ladder_from`] and as one `contains` check
/// inside another test. Neither survives somebody adding a rung to a list, and
/// the rung now exists - `cleaner_core::engines::flux` is callable today. So it
/// is pinned three ways, because there are three ways in:
///
/// 1. **Nothing starts there.** [`engine_for`] names a region's first rung.
/// 2. **Nothing escalates there.** [`ladder_from`] is the escalation's list.
/// 3. **Nothing renders there.** [`render_rung`] used to answer an unreachable
///    rung with a *wildcard* that produced rung 0's pixels - so a `Flux` that
///    somehow arrived would have been written to the manifest as a FLUX patch
///    over a planar fill. It refuses now.
///
/// **And [`HIGHEST_AUTOMATIC`] is LaMa**, which is what makes the three checks
/// above statements about the whole ladder rather than about two named rungs.
/// MI-GAN held rung 3 and the line between automatic and not ran above it; the
/// rung is gone and the line has come down to rung 2, so the strongest thing a
/// batch job can spend is the inpainter. Asserted on the constant *and* on
/// every list this loop produces, because the constant is the rule and the
/// lists are whether anything reads it.
#[test]
fn rung_3a_is_not_reachable_from_an_automatic_run() {
    let sources = [
        fixtures::by_name("l8").raster,
        fixtures::by_name("l16").raster,
        fixtures::by_name("rgb8").raster,
        fixtures::by_name("bitonal").raster,
        fixtures::by_name("indexed-p").raster,
    ];
    let rungs = [Engine::Fill, Engine::Lama, Engine::Flux, Engine::Cloud];

    // The constant itself, and the predicate every filter reads it through.
    assert_eq!(HIGHEST_AUTOMATIC, Engine::Lama);
    for engine in [Engine::Fill, Engine::Lama] {
        assert!(reachable_automatically(engine), "{engine:?}");
    }
    for engine in [Engine::Flux, Engine::Cloud] {
        assert!(!reachable_automatically(engine), "{engine:?}");
    }

    for page in &sources {
        for start in rungs {
            for ceiling in rungs {
                let ladder = ladder_from(start, ceiling, page);
                assert!(
                    !ladder.contains(&Engine::Flux),
                    "rung 3a reached the ladder from {start:?} under {ceiling:?}"
                );
                assert!(
                    !ladder.contains(&Engine::Cloud),
                    "rung 4 reached the ladder from {start:?} under {ceiling:?}"
                );
                // The same statement without naming a rung, so that a rung
                // added above LaMa is caught by this test rather than by the
                // two `contains` checks somebody would have to remember to
                // extend.
                for engine in &ladder {
                    assert!(
                        rung(*engine) <= rung(HIGHEST_AUTOMATIC),
                        "{engine:?} is above the automatic ceiling, from {start:?} under {ceiling:?}"
                    );
                }
            }
        }
    }

    // 1 - and the router is asked at every ceiling, including the two that
    // name rung 3a and above, because "highest permitted rung wins" is the
    // reading that would get this wrong.
    for ceiling in rungs {
        for route in [Route::Fill, Route::Inpaint] {
            assert_ne!(engine_for(route, ceiling), Some(Engine::Flux));
            assert_ne!(engine_for(route, ceiling), Some(Engine::Cloud));
        }
    }

    // And the stored setting cannot lift it either: `effective_ceiling` is
    // free to answer `Flux` - the rung is local, and a patch made by hand at
    // that rung is a real patch - but a ceiling of `Flux` buys an automatic run
    // exactly what a ceiling of `Lama` buys it.
    let page = &sources[0];
    assert_eq!(
        ladder_from(Engine::Fill, effective_ceiling(Some("flux"), None), page),
        ladder_from(Engine::Fill, Engine::Lama, page),
    );

    // 3 - the rung that cannot be reached is refused rather than filled.
    let page = gray_page(|x, y| if tone(x, y) { 40 } else { 235 });
    let fitted = fitted_over(&page, Rect::new(60, 60, 40, 30));
    for engine in [Engine::Flux, Engine::Cloud] {
        let rendered = render_rung(&mut no_rung_two(), engine, &page, &fitted)
            .expect("an unreachable rung faulted rather than refusing");
        match rendered {
            Rendered::Refused(key) => assert_eq!(key, "decline.reason.rungUnavailable"),
            Rendered::Made(_) => panic!("{engine:?} produced a patch from an automatic run"),
        }
    }
}

/// The ceiling the scheduler is given is the ceiling the cleaner sees, on every
/// page. A run that lifted it after the first page would be the same defect
/// with a longer fuse.
#[test]
fn every_page_of_a_run_is_cleaned_under_the_same_ceiling() {
    let scratch = Scratch::new("ceiling-per-page");
    let (library, _, chapter_id) = a_chapter(&scratch, 3);
    let recorder = Recorder::new();
    let mut stub = Stub::new(1, 0);
    let ceilings = Arc::clone(&stub.ceilings);
    execute(
        "run-1",
        &chapter_id,
        &plan(&library, "chapter", &chapter_id, None).unwrap(),
        &mut stub,
        Engine::Fill,
        &recorder.cancel,
        &recorder.sink(),
    );
    assert_eq!(*ceilings.lock().unwrap(), vec![Engine::Fill; 3]);
}

/* ------------------------------------------------------------------ */
/* Try, escalate, decline                                              */
/* ------------------------------------------------------------------ */

// The escalation loop is written against [`Rung2`] rather than against
// [`Pipeline`] for the reason this file's header gives about [`Cleaner`]: rung
// 2's weights are not in the repository, and every rule below is a property
// of the loop rather than of the model.
// So the loop is exercised here with the session deliberately absent, and
// `Rung2::state` says afterwards whether one was ever asked for - which is the
// memory claim itself, not a proxy for it.

/// A rung 2 that is not on this machine: every checkout that has not run
/// `scripts/fetch-models.sh`, and every CI runner.
fn no_rung_two() -> Rung2 {
    Rung2::new(Path::new("no-weights-live-here"), Preference::CpuOnly)
}

const SYNTHETIC: u32 = 200;

/// An 8 px halftone grid - `fit::ring`'s own screentone, which routes to the
/// ladder by construction.
fn tone(x: u32, y: u32) -> bool {
    x % 8 < 3 && y % 8 < 3
}

fn gray_page(f: impl Fn(u32, u32) -> u8) -> Raster {
    let mut data = Vec::with_capacity((SYNTHETIC * SYNTHETIC) as usize);
    for y in 0..SYNTHETIC {
        for x in 0..SYNTHETIC {
            data.push(f(x, y));
        }
    }
    Raster {
        width: SYNTHETIC,
        height: SYNTHETIC,
        mode: ColorMode::Gray,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data,
    }
}

/// The same halftone at one bit per sample, packed the way a 1-bit PNG is.
fn bitonal_page() -> Raster {
    let stride = (SYNTHETIC as usize).div_ceil(8);
    let mut data = vec![0u8; stride * SYNTHETIC as usize];
    for y in 0..SYNTHETIC {
        for x in 0..SYNTHETIC {
            if !tone(x, y) {
                data[y as usize * stride + x as usize / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    Raster {
        width: SYNTHETIC,
        height: SYNTHETIC,
        mode: ColorMode::Gray,
        depth: BitDepth::One,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data,
    }
}

fn fitted_over(page: &Raster, rect: Rect) -> fit::Fitted {
    fit::fit(
        page,
        &Mask::filled(rect),
        1.0,
        fit::page_noise_sigma(page),
        &EdgeMap::none(page.width, page.height),
        false,
    )
}

fn attempt(rung2: &mut Rung2, page: &Raster, fitted: &fit::Fitted, ceiling: Engine) -> Attempt {
    clean_region(
        rung2,
        page,
        fitted,
        ceiling,
        // No pick: these are assertions about the route's own rung and the
        // escalation above it, which is what an unpicked region gets.
        None,
        fit::page_noise_sigma(page),
    )
    .expect("no rung faulted")
}

/// The memory claim, asserted rather than described. A manga-LaMa session
/// costs 2.1 s and about 510 MB, and a page of flat-paper dialogue must not
/// pay a byte of it.
#[test]
fn a_page_of_flat_paper_never_pays_for_a_lama_session() {
    let page = gray_page(|_, _| 246);
    let mut rung2 = no_rung_two();
    for x in [40, 88, 140] {
        let fitted = fitted_over(&page, Rect::new(x, 90, 24, 20));
        assert_eq!(fitted.route, Route::Fill);
        match attempt(&mut rung2, &page, &fitted, Engine::Flux) {
            Attempt::Cleaned(made, verdict) => {
                assert_eq!(made.engine, Engine::Fill);
                assert_eq!(verdict.outcome, quality::Outcome::Accepted, "{verdict:?}");
                // No model ran, so there is no digest and there are no tiles.
                assert_eq!(made.model_sha256, None);
                assert_eq!(made.tiles, None);
                assert_eq!(made.provider, None);
            }
            Attempt::Declined(reason) => panic!("flat paper was declined: {reason}"),
        }
    }
    assert!(
        matches!(rung2.state, Session::Unopened),
        "a page of flat-paper dialogue opened an inpainter"
    );
}

/// A region §5 sent to the ladder, on a machine that has no ladder to send it
/// to. It is **left alone** rather than filled by a rung that was already
/// refused - and the session is asked for once, not once per region.
#[test]
fn a_ladder_region_with_no_rung_two_is_left_alone_and_says_why() {
    let page = gray_page(|x, y| if tone(x, y) { 40 } else { 250 });
    let fitted = fitted_over(&page, Rect::new(88, 90, 24, 20));
    assert_eq!(fitted.route, Route::Inpaint, "the screentone did not reach the ladder");

    let mut rung2 = no_rung_two();
    match attempt(&mut rung2, &page, &fitted, Engine::Flux) {
        Attempt::Declined(reason) => assert_eq!(reason, "decline.reason.rungUnavailable"),
        Attempt::Cleaned(made, _) => {
            panic!("a screentone region was filled by {:?} with no inpainter", made.engine)
        }
    }
    assert!(matches!(rung2.state, Session::Unavailable), "the failure was not remembered");
}

/// The same region under a ceiling below rung 2: declined **without asking any
/// engine at all**, which is why it carries the reason it does rather than
/// §6's.
///
/// It does not downgrade to rung 0, and that is deliberate: a flat plate over a halftone is a hole in the tone, and §6's
/// metric passes it - the plate has no strokes to fail the edge test and its
/// level sits inside the tone's own p5..p95.
#[test]
fn a_ladder_region_under_a_low_ceiling_declines_without_asking_any_engine() {
    let page = gray_page(|x, y| if tone(x, y) { 40 } else { 250 });
    let fitted = fitted_over(&page, Rect::new(88, 90, 24, 20));
    let mut rung2 = no_rung_two();
    let ceiling = Engine::Fill;
    match attempt(&mut rung2, &page, &fitted, ceiling) {
        Attempt::Declined(reason) => assert_eq!(reason, "decline.reason.rungUnavailable"),
        Attempt::Cleaned(made, _) => {
            panic!("{:?} filled a screentone region at ceiling {ceiling:?}", made.engine)
        }
    }
    assert!(matches!(rung2.state, Session::Unopened), "a capped run reached for the session");
}

/// **A bitonal scan cleaned nothing at all.** Every annulus on a 1-bit source is
/// dither, so §5 routed every region of the page to the ladder, and rung 2
/// declines a 1-bit source outright ([`lama::Decline::Depth`]) - the engine
/// right and the router wrong. Rung 0's planar fill serves bitonal and always
/// has.
#[test]
fn a_bitonal_page_cleans_at_rung_zero_rather_than_declining_wholesale() {
    let page = bitonal_page();
    let mut rung2 = no_rung_two();
    for x in [40, 88, 140] {
        let fitted = fitted_over(&page, Rect::new(x, 90, 24, 20));
        match attempt(&mut rung2, &page, &fitted, Engine::Flux) {
            Attempt::Cleaned(made, _) => assert_eq!(made.engine, Engine::Fill),
            Attempt::Declined(reason) => panic!("a bitonal region was declined: {reason}"),
        }
    }
    assert!(
        matches!(rung2.state, Session::Unopened),
        "a bitonal page reached for the one rung that refuses it"
    );
}

/// §6's two tests are two facts, and a reviewer acting on them acts
/// differently: one says the engine put strokes on the paper, the other says it
/// put a tone that is not the paper's.
#[test]
fn the_two_halves_of_the_quality_metric_are_two_reasons() {
    assert_eq!(decline_key(quality::Cause::EdgeEnergy), "decline.reason.edgeEnergy");
    assert_eq!(decline_key(quality::Cause::Histogram), "decline.reason.histogram");
    assert_ne!(decline_key(quality::Cause::EdgeEnergy), decline_key(quality::Cause::Histogram));
}

/* ------------------------------------------------------------------ */
/* What reaches the seam                                               */
/* ------------------------------------------------------------------ */

/// `fill_mode` and `elapsed_ms` were reconstructed - one
/// guessed from the rung, the other defaulted to zero - because nothing wrote
/// them. The run writes both into `params_snapshot`, and this pins that the
/// recorded values are what reach the seam.
#[test]
fn a_recorded_fill_mode_and_elapsed_time_reach_the_mask_on_the_seam() {
    let scratch = Scratch::new("provenance");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let recorder = Recorder::new();
    execute(
        "run-1",
        &chapter_id,
        &plan(&library, "chapter", &chapter_id, None).unwrap(),
        &mut Stub::new(1, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    let region = recorder
        .events()
        .into_iter()
        .find_map(|event| match event {
            Event::RegionDone { region, .. } => Some(*region),
            _ => None,
        })
        .expect("no region reached the seam");
    let mask = region.mask.expect("a cleaned region with no mask");
    assert_eq!(mask.fill_mode, "match-surround");
    assert_eq!(mask.elapsed_ms, 41);
    assert_eq!(region.outcome, "cleaned");
    assert_eq!(region.id, format!("{chapter_id}-p001-r0"));
    assert_eq!(mask.id, format!("{chapter_id}-p001-r0-m1"));
}

/// The snapshot the real pipeline writes, over a ring sampled from a fixture.
/// Same two fields, at the other end of the same path.
#[test]
fn the_pipeline_writes_the_fill_mode_and_the_time_it_took() {
    let page = fixtures::by_name("l8").raster;
    let mask = Mask::filled(Rect::new(4, 4, 6, 6));
    let edges = EdgeMap::none(page.width, page.height);
    let paper = cleaner_core::fit::ring::paper_annulus(&page, &mask, 0, &edges);
    let fitted = fit::Fitted {
        ring: cleaner_core::fit::ring::sample_over(&page, &paper),
        paper,
        // Rung 0's own mask, and a rung-0 snapshot: `ink` is only ever read by a
        // model rung, and here it is the whole of the mask anyway.
        ink: mask.clone(),
        mask,
        route: Route::Fill,
        thickness: 9,
        best_deviation: 1.5,
    };
    let measured = quality::assess(
        &page,
        &fitted.mask,
        &cleaner_core::engines::fill::render(&page, &fitted),
        0.0,
    );
    let snapshot = params_snapshot(
        Engine::Fill,
        &fitted,
        Duration::from_millis(37),
        EdgePad::Replicate,
        None,
        &measured,
    );
    assert_eq!(snapshot["fill_mode"], "match-surround");
    assert_eq!(snapshot["elapsed_ms"], 37);
    assert_eq!(snapshot["rung"], "ladder.rung.fill");
    assert_eq!(snapshot["thickness"], 9);
    assert_eq!(snapshot["source"], "auto");
    // Which edge pad was used, recorded in `params_snapshot`.
    assert_eq!(snapshot["edge_pad"], "edge-replicate");
    // No model ran, so there are no tiles - absent rather than zero, which
    // would read as a model that did nothing.
    assert_eq!(snapshot.get("tiles"), None);
    assert_eq!(
        params_snapshot(Engine::Fill, &fitted, Duration::ZERO, EdgePad::None, None, &measured)
            ["edge_pad"],
        "none"
    );

    // Rung 2's half of the same record: a reconstruction, and the number of
    // model runs it cost.
    let inpainted =
        params_snapshot(Engine::Lama, &fitted, Duration::ZERO, EdgePad::None, Some(4), &measured);
    assert_eq!(inpainted["fill_mode"], "reconstruct");
    assert_eq!(inpainted["tiles"], 4);
}

/// The metric's silence is recorded rather than passed off as a verdict -
/// "a check that silently answers accepted" is the failure this whole family
/// is at risk of.
#[test]
fn a_patch_nothing_could_measure_says_so_in_its_snapshot() {
    let page = fixtures::by_name("l8").raster;
    let mask = Mask::filled(Rect::new(4, 4, 6, 6));
    let edges = EdgeMap::none(page.width, page.height);
    let paper = cleaner_core::fit::ring::paper_annulus(&page, &mask, 0, &edges);
    let fitted = fit::Fitted {
        ring: cleaner_core::fit::ring::sample_over(&page, &paper),
        paper,
        // Rung 0's own mask, and a rung-0 snapshot: `ink` is only ever read by a
        // model rung, and here it is the whole of the mask anyway.
        ink: mask.clone(),
        mask,
        route: Route::Fill,
        thickness: 9,
        best_deviation: 1.5,
    };
    let measured = quality::assess(
        &page,
        &fitted.mask,
        &cleaner_core::engines::fill::render(&page, &fitted),
        0.0,
    );
    assert_eq!(
        params_snapshot(Engine::Fill, &fitted, Duration::ZERO, EdgePad::None, None, &measured)
            ["quality"],
        "measured"
    );

    let bitonal = fixtures::by_name("bitonal").raster;
    let unmeasured = quality::assess(&bitonal, &fitted.mask, &page, 0.0);
    assert_eq!(
        params_snapshot(Engine::Fill, &fitted, Duration::ZERO, EdgePad::None, None, &unmeasured)
            ["quality"],
        "unmeasured"
    );
}

/// A region the gate held back is written down and reaches the seam as a
/// gate-skipped region with the cause the gate gave - it is the review list's
/// other half, and a page that silently lost it would show a clean page with no
/// record that anything was deliberately left alone.
#[test]
fn a_region_the_gate_held_back_is_recorded_and_flagged() {
    let scratch = Scratch::new("gated");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let recorder = Recorder::new();
    let summary = execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(0, 2),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    assert_eq!(summary.regions_cleaned, 0, "a held-back region was counted as cleaned");
    let job = Job::open(&entries[0].job_path).unwrap();
    assert_eq!(job.project.regions_untouched.len(), 2);
    assert_eq!(job.project.counters.gate_dropped, 2);

    let page = match recorder.events().into_iter().find(|e| kind_of(e) == "page-done") {
        Some(Event::PageDone { page, .. }) => *page,
        _ => panic!("no page-done"),
    };
    assert_eq!(page.regions.len(), 2);
    assert!(page.regions.iter().all(|region| region.outcome == "gate-skipped"));
    assert!(page.regions.iter().all(|region| region.gate_skip_cause == Some("not-japanese")));
    // Held back is not cleaned, and a page of held-back regions is not a
    // cleaned page in the sense of having had anything done to it - but it has
    // been examined, so it is finished.
    assert_eq!(page.status, "cleaned");
    assert!(plan(&library, "chapter", &chapter_id, None).unwrap().is_empty());
}

/// A page that could not be read is counted, reported finished so the `●` comes
/// off it, and left in the queue for the next run.
#[test]
fn a_page_that_could_not_be_cleaned_is_counted_and_stays_queued() {
    let scratch = Scratch::new("errored");
    let (library, _, chapter_id) = a_chapter(&scratch, 2);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let mut stub = Stub::new(1, 0);
    stub.fails = vec![library::page_id(&chapter_id, 0)];

    let recorder = Recorder::new();
    let summary = execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut stub,
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    assert_eq!(summary.errored, 1);
    assert_eq!(summary.pages_cleaned, 1);
    assert_eq!(summary.reason, "completed");
    assert_eq!(recorder.kinds().iter().filter(|k| **k == "page-done").count(), 2);
    let job = Job::open(&entries[0].job_path).unwrap();
    assert_eq!(job.project.counters.errored, 1);
    assert_eq!(
        plan(&library, "chapter", &chapter_id, None)
            .unwrap()
            .iter()
            .map(|entry| entry.page_index)
            .collect::<Vec<_>>(),
        vec![0]
    );
}

/// Every region is flushed as it completes, so a crash mid-run loses the region
/// in flight and nothing else. Read back off disk rather than out of the job in
/// memory, because that is the only version a crash would leave.
#[test]
fn every_completed_region_is_on_disk_before_the_next_one_starts() {
    let scratch = Scratch::new("flush");
    let (library, _, chapter_id) = a_chapter(&scratch, 2);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let job_path = entries[0].job_path.clone();
    let counted = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&counted);
    let cancel = AtomicBool::new(false);
    execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(2, 0),
        Engine::Flux,
        &cancel,
        &|event| {
            if kind_of(&event) == "region-done" {
                let on_disk = Job::open(&job_path).unwrap().project.patches.len();
                seen.lock().unwrap().push(on_disk);
            }
        },
    );
    assert_eq!(
        *counted.lock().unwrap(),
        vec![1, 2, 3, 4],
        "a region was reported before it was written down"
    );
}

/* ------------------------------------------------------------------ */
/* A run that does not come back                                       */
/* ------------------------------------------------------------------ */

/// A panic in the pipeline is still a run that finished, and the seam's rule
/// says so: **exactly one `run-finished`, whatever happened**. Without the
/// guard the event never went out - the page kept its `●` for the session, the
/// active slot stayed claimed so every later `runClean` answered
/// `alreadyRunning`, and `cancelRun` waited the full `CANCEL_WAIT` before naming
/// a run that had already stopped.
#[test]
fn a_panicking_pipeline_still_finishes_the_run_and_frees_the_scheduler() {
    let _exclusive = exclusively();
    let scratch = Scratch::new("panic");
    let (library, _, chapter_id) = a_chapter(&scratch, 3);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let mut stub = Stub::new(1, 0);
    stub.panics = vec![library::page_id(&chapter_id, 1)];

    let recorder = Recorder::new();
    let worker = a_worker(&chapter_id, &recorder);
    let summary = quietly(|| worker.run(&entries, &mut stub, Engine::Flux, &recorder.sink()));

    let finished = recorder.kinds().iter().filter(|kind| **kind == "run-finished").count();
    assert_eq!(finished, 1, "a panicked run emitted {finished} run-finished events");
    assert_eq!(
        recorder.kinds().last(),
        Some(&"notice"),
        "the run ended on something other than its notice"
    );
    assert_eq!(summary.reason, "cancelled", "a panicked run has not completed");
    assert_eq!(summary.next_page_index, Some(1), "the page it stopped inside is the resume point");
    assert_eq!(summary.pages_cleaned, 1, "the page before it was still finished");

    // The slot is free, so the next run can start. This is the whole of what
    // `alreadyRunning` for the rest of the session was.
    assert!(
        active().lock().unwrap().is_empty(),
        "a panicked run kept the scheduler for the rest of the session"
    );
    assert!(*worker.finished.0.lock().unwrap(), "a cancelRun would have waited out the timeout");

    // And the manifest can say where to pick up, which needs the walk's own
    // `close` to have been stood in for.
    assert_eq!(Job::open(&entries[0].job_path).unwrap().project.interrupted_at, Some(1));
    assert_eq!(
        plan(&library, "chapter", &chapter_id, None)
            .unwrap()
            .iter()
            .map(|entry| entry.page_index)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "the page the panic landed on was not left in the queue"
    );
}

/// The exit `execute` cannot catch: a subscriber that panics on the way out.
/// The slot is handed back by a guard rather than by a line at the end, so it
/// is handed back on this path too.
#[test]
fn a_panic_on_the_way_out_still_hands_the_scheduler_back() {
    let _exclusive = exclusively();
    let scratch = Scratch::new("panic-sink");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let recorder = Recorder::new();
    let worker = a_worker(&chapter_id, &recorder);

    let panicked = quietly(|| {
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            worker.run(&entries, &mut Stub::new(1, 0), Engine::Flux, &|event| {
                assert_ne!(kind_of(&event), "run-finished", "a sink that throws on the way out");
            })
        }))
    });
    assert!(panicked.is_err(), "the sink did not panic and the guard was not exercised");
    assert!(
        active().lock().unwrap().is_empty(),
        "a panic outside the scheduler kept the slot for the rest of the session"
    );
}

/* ------------------------------------------------------------------ */
/* One writer per job                                                  */
/* ------------------------------------------------------------------ */

/// The run holds the job it is writing for as long as it has it open, so no
/// other command in the window can read it, mutate it and flush over what the
/// run has since written. The exposure is what made this worth closing: one
/// `Job` is held for a **whole chapter** and the manifest is rewritten after
/// every region.
#[test]
fn the_run_holds_its_job_for_as_long_as_it_is_open() {
    let scratch = Scratch::new("job-held");
    let (library, _, chapter_id) = a_chapter(&scratch, 2);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let job_path = entries[0].job_path.clone();
    assert!(job_is_free(&job_path), "the job was held before the run started");

    let free_during = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&free_during);
    let cancel = AtomicBool::new(false);
    execute(
        "backend-run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(1, 0),
        Engine::Flux,
        &cancel,
        &|event| {
            if kind_of(&event) == "region-done" {
                seen.lock().unwrap().push(job_is_free(&job_path));
            }
        },
    );
    assert_eq!(*free_during.lock().unwrap(), vec![false, false], "the run left its job writable");
    assert!(job_is_free(&job_path), "the run kept the job after it had finished with it");
}

/// And a second writer waits rather than interleaving. Within one process;
/// the tests below take the same rule across two.
#[test]
fn a_second_writer_waits_for_the_job_rather_than_interleaving() {
    let scratch = Scratch::new("job-lock");
    let path = scratch.join("c1.mtclean");
    let order = Arc::new(Mutex::new(Vec::new()));

    let held = lock_job(&path).unwrap();
    let second = {
        let path = path.clone();
        let order = Arc::clone(&order);
        std::thread::spawn(move || {
            let _held = lock_job(&path).unwrap();
            order.lock().unwrap().push("second");
        })
    };
    // Long enough that a lock that did not hold would let the second writer
    // through first, which is the failure this pins.
    std::thread::sleep(Duration::from_millis(80));
    order.lock().unwrap().push("first");
    drop(held);
    second.join().unwrap();

    assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    assert!(job_is_free(&path), "the lock was not given back");
}

/// A writer that skips the lock can still save under a run: an older build,
/// say. The run's next save is refused rather than rolling that back, and the
/// run reads the job again and goes on with the next page.
#[test]
fn a_run_does_not_roll_back_a_save_it_did_not_see_and_goes_on() {
    let scratch = Scratch::new("run-stale");
    let (library, _, chapter_id) = a_chapter(&scratch, 3);
    let job_path = library.resolve_chapter(&chapter_id).unwrap();
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let written = AtomicBool::new(false);
    let cancel = AtomicBool::new(false);
    let summary = execute(
        "backend-run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(2, 0),
        Engine::Flux,
        &cancel,
        &|event| {
            if kind_of(&event) == "region-done" && !written.swap(true, Ordering::SeqCst) {
                // Raw bytes, no lock: the outside writer.
                let mut raw: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&job_path).unwrap()).unwrap();
                raw["app_version"] = serde_json::json!("outside-writer");
                std::fs::write(&job_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
            }
        },
    );

    let job = Job::open(&job_path).unwrap();
    assert_eq!(job.project.app_version, "outside-writer", "the run saved over a manifest it had not read");
    let ids: Vec<&str> = job.project.patches.iter().map(|row| row.id.as_str()).collect();
    let first = library::page_id(&chapter_id, 0);
    assert!(ids.contains(&region_id(&first, 0).as_str()), "the region saved before the outside write went");
    assert!(!ids.contains(&region_id(&first, 1).as_str()), "a refused save landed");
    for page in [1, 2] {
        let page_id = library::page_id(&chapter_id, page);
        assert!(ids.contains(&region_id(&page_id, 0).as_str()) && ids.contains(&region_id(&page_id, 1).as_str()),
            "the run did not go on after reading the job again: {ids:?}");
    }
    assert!(!job.project.examined.contains(&0), "the page with the refused save is not finished");
    assert!(job.project.examined.contains(&1) && job.project.examined.contains(&2));
    assert_eq!(summary.errored, 1);
    assert_eq!(job.project.interrupted_at, None);
}

/// An outside save on the last page the run reaches: the run's own last
/// save is refused, and the chapter is still closed, on the manifest read
/// again. A completed run clears the resume point Home would otherwise offer.
#[test]
fn a_run_whose_last_save_went_stale_still_clears_its_resume_point() {
    let scratch = Scratch::new("run-stale-last");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let job_path = library.resolve_chapter(&chapter_id).unwrap();
    {
        let mut job = Job::open(&job_path).unwrap();
        job.mark_interrupted(Some(0)).unwrap();
    }
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let written = AtomicBool::new(false);
    let cancel = AtomicBool::new(false);
    let summary = execute("backend-run-1", &chapter_id, &entries, &mut Stub::new(2, 0), Engine::Flux, &cancel, &|event| {
        if kind_of(&event) == "region-done" && !written.swap(true, Ordering::SeqCst) {
            let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&job_path).unwrap()).unwrap();
            raw["app_version"] = serde_json::json!("outside-writer");
            std::fs::write(&job_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        }
    });

    let job = Job::open(&job_path).unwrap();
    assert_eq!(job.project.app_version, "outside-writer");
    assert_eq!(summary.reason, "completed");
    assert_eq!(job.project.interrupted_at, None, "the finished run left its old resume point behind");
}

/// The same, cancelled: the resume point is written on what the other writer
/// saved, rather than not at all.
#[test]
fn a_cancelled_run_whose_save_went_stale_still_records_where_it_stopped() {
    let scratch = Scratch::new("run-stale-cancel");
    let (library, _, chapter_id) = a_chapter(&scratch, 2);
    let job_path = library.resolve_chapter(&chapter_id).unwrap();
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let written = AtomicBool::new(false);
    let cancel = AtomicBool::new(false);
    let summary = execute("backend-run-1", &chapter_id, &entries, &mut Stub::new(2, 0), Engine::Flux, &cancel, &|event| {
        if kind_of(&event) == "region-done" && !written.swap(true, Ordering::SeqCst) {
            let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&job_path).unwrap()).unwrap();
            raw["app_version"] = serde_json::json!("outside-writer");
            std::fs::write(&job_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
            cancel.store(true, Ordering::SeqCst);
        }
    });

    let job = Job::open(&job_path).unwrap();
    assert_eq!(job.project.app_version, "outside-writer");
    assert_eq!(summary.reason, "cancelled");
    // Cancelled part way through page 0, which is where a resume picks up.
    assert_eq!(summary.next_page_index, Some(0));
    assert_eq!(job.project.interrupted_at, Some(0), "Home has no resume point for the cancelled run");
}

/// Deleting a chapter waits for its writer, so a writer that was part way
/// through cannot put its sidecar back after the delete. The lock file goes
/// with the chapter.
#[test]
fn deleting_a_chapter_waits_for_its_writer_and_leaves_nothing_behind() {
    let scratch = Scratch::new("delete-under-writer");
    let (library, project_id, chapter_id) = a_chapter(&scratch, 1);
    let job_path = library.resolve_chapter(&chapter_id).unwrap();
    let sidecar = cleaner_core::project::sidecar_dir(&job_path);
    let (held, held_rx) = std::sync::mpsc::channel();
    let writer = {
        let job_path = job_path.clone();
        let sidecar = sidecar.clone();
        std::thread::spawn(move || {
            let _lock = lock_job(&job_path).unwrap();
            held.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            // A writer past its stale check, writing a buffer.
            std::fs::create_dir_all(&sidecar).unwrap();
            std::fs::write(sidecar.join("late.buf"), b"pixels").unwrap();
        })
    };
    held_rx.recv().unwrap();
    let deleted = library.delete_chapter(&project_id, &chapter_id, false).unwrap();
    writer.join().unwrap();

    assert!(matches!(deleted, library::ChapterDeleted::Deleted { .. }));
    assert!(!job_path.exists());
    assert!(!sidecar.exists(), "a writer's sidecar outlived the chapter");
    assert!(!cleaner_core::project::lock::lock_path(&job_path).exists(), "the chapter's lock file outlived it");
}

/* ------------------------------------------------------------------ */
/* Two processes over one library                                      */
/* ------------------------------------------------------------------ */

/* The observed rollback: the GUI and a headless runner were both open on one
 * library. The GUI re-cleaned one region with LaMa, holding the manifest it had
 * read for about seven seconds, while the runner committed two cloud patches
 * under its own lock; the GUI's save then wrote its older manifest over both.
 * These tests put that shape into two real processes: the test binary starts
 * itself again, filtered to `job_lock_child_process`, with the second writer's
 * part named in the environment. */

const CHILD_ROLE: &str = "MC_JOB_LOCK_CHILD_ROLE";
const CHILD_JOB: &str = "MC_JOB_LOCK_CHILD_JOB";
const CHILD_SIGNALS: &str = "MC_JOB_LOCK_CHILD_SIGNALS";
const CHILD_PREFIX: &str = "MC_JOB_LOCK_CHILD_PREFIX";

/// Start this test binary again as the second process, playing `role`.
fn spawn_child(role: &str, job: &Path, signals: &Path, prefix: &str) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["run::tests::job_lock_child_process", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ROLE, role)
        .env(CHILD_JOB, job)
        .env(CHILD_SIGNALS, signals)
        .env(CHILD_PREFIX, prefix)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("second process")
}

fn signal(signals: &Path, name: &str) {
    std::fs::write(signals.join(name), b"").unwrap();
}

/// Wait for a signal file, up to `limit`. `false` when it never came.
fn wait_for_signal(signals: &Path, name: &str, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if signals.join(name).exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// Not a test of its own. With no role in the environment it does nothing;
/// with one, it is the other process of the tests below.
#[test]
fn job_lock_child_process() {
    let Ok(role) = std::env::var(CHILD_ROLE) else { return };
    let path = PathBuf::from(std::env::var(CHILD_JOB).unwrap());
    let signals = PathBuf::from(std::env::var(CHILD_SIGNALS).unwrap());
    let prefix = std::env::var(CHILD_PREFIX).unwrap_or_default();
    match role.as_str() {
        // The GUI's hand edit: the job read under the lock, a slow render,
        // then the save. The render waits for the other writer's commit, or
        // gives up after a while when a lock keeps that writer out.
        "slow-edit" => {
            let _lock = lock_job(&path).unwrap();
            let mut job = Job::open(&path).unwrap();
            signal(&signals, "opened");
            wait_for_signal(&signals, "parent-committed", Duration::from_millis(1500));
            job.complete_region(0, &a_patch("gui-edit", 1, Engine::Lama), None).unwrap();
        }
        // A writer that never takes the lock, as an older build or another
        // tool would: its save must be refused rather than land.
        "unlocked-edit" => {
            let mut job = Job::open(&path).unwrap();
            signal(&signals, "opened");
            assert!(wait_for_signal(&signals, "parent-committed", Duration::from_secs(20)));
            let outcome = job.complete_region(0, &a_patch("unlocked-edit", 1, Engine::Lama), None);
            signal(&signals, if outcome.is_ok() { "written" } else { "refused" });
        }
        // One of several writers committing as fast as they can.
        "writer" => {
            assert!(wait_for_signal(&signals, "go", Duration::from_secs(20)));
            for n in 0..SIMULTANEOUS_COMMITS {
                let _lock = lock_writer(&path);
                let mut job = Job::open(&path).unwrap();
                job.complete_region(0, &a_patch(&format!("{prefix}-{n}"), n, Engine::Fill), None).unwrap();
            }
        }
        // A detection's masks written, its manifest row not yet: the window
        // `store_detection` has between its two halves.
        "unflushed-detection" => {
            let _lock = lock_job(&path).unwrap();
            let detections = cleaner_core::project::sidecar_dir(&path).join("detections");
            std::fs::create_dir_all(&detections).unwrap();
            std::fs::write(detections.join("pending-d9.mask"), b"mask").unwrap();
            std::fs::write(detections.join("pending-d9.ink"), b"ink").unwrap();
            signal(&signals, "written");
            wait_for_signal(&signals, "parent-read", Duration::from_secs(20));
        }
        // A process holding the job and doing nothing else with it, as a
        // headless run holds its chapter from the first page to the last.
        "holder" => {
            let _lock = lock_job(&path).unwrap();
            signal(&signals, "held");
            wait_for_signal(&signals, "release", Duration::from_secs(30));
        }
        other => panic!("unknown child role {other}"),
    }
}

/// A chapter held by another process, and a second process to hold it.
fn held_elsewhere(scratch: &Scratch, pages: usize) -> (Library, String, String, PathBuf, PathBuf, std::process::Child) {
    let (library, project_id, chapter_id) = a_chapter(scratch, pages);
    let path = library.resolve_chapter(&chapter_id).unwrap();
    let signals = scratch.join("signals");
    std::fs::create_dir_all(&signals).unwrap();
    let child = spawn_child("holder", &path, &signals, "");
    assert!(wait_for_signal(&signals, "held", Duration::from_secs(30)), "the second process never took the job");
    (library, project_id, chapter_id, path, signals, child)
}

/// A command does not wait out another process's run: it gives up after a
/// few seconds with an error whose code the interface reads, and changes
/// nothing on the way.
#[test]
fn a_command_gives_up_on_a_chapter_another_process_holds() {
    let scratch = Scratch::new("two-process-busy-command");
    let (library, project_id, chapter_id, path, signals, mut child) = held_elsewhere(&scratch, 1);

    let started = Instant::now();
    let refused = library.delete_chapter(&project_id, &chapter_id, false).err();
    let waited = started.elapsed();
    signal(&signals, "release");
    assert!(child.wait().unwrap().success(), "the second process failed");

    let refused = refused.expect("a delete went through another process's lock");
    assert!(matches!(refused, LibraryError::Busy { .. }), "{refused}");
    assert_eq!(refused.reason_key(), "notice.job.busy");
    assert!(String::from(refused).starts_with("job_busy: "));
    assert!(waited >= cleaner_core::project::lock::PATIENCE && waited < Duration::from_secs(20), "{waited:?}");
    assert!(path.exists(), "the chapter was deleted anyway");
    assert!(library.list_projects().unwrap()[0].chapters.iter().any(|chapter| chapter.id == chapter_id));
}

/// A run over a chapter another process holds waits once, says so once, and
/// leaves every page of it for a later run.
#[test]
fn a_run_leaves_a_chapter_another_process_holds_and_says_so() {
    let scratch = Scratch::new("two-process-busy-run");
    let (library, _, chapter_id, path, signals, mut child) = held_elsewhere(&scratch, 3);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let recorder = Recorder::new();

    let started = Instant::now();
    let summary = execute("backend-run-1", &chapter_id, &entries, &mut Stub::new(1, 0), Engine::Flux, &recorder.cancel, &recorder.sink());
    let waited = started.elapsed();
    signal(&signals, "release");
    assert!(child.wait().unwrap().success(), "the second process failed");

    let busy = recorder.events().iter()
        .filter(|event| matches!(event, Event::Notice { key, .. } if key == "notice.job.busy"))
        .count();
    assert_eq!(busy, 1, "{:?}", recorder.kinds());
    assert!(!recorder.kinds().contains(&"page-started"));
    assert_eq!(summary.errored, 3);
    assert!(waited < RUN_PATIENCE * 2, "the run waited once per page: {waited:?}");
    assert!(Job::open(&path).unwrap().project.examined.is_empty());
}

/// And the run's cancel ends that wait at once.
#[test]
fn a_cancel_ends_a_runs_wait_for_a_chapter_another_process_holds() {
    let scratch = Scratch::new("two-process-busy-cancel");
    let (library, _, chapter_id, _, signals, mut child) = held_elsewhere(&scratch, 2);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let recorder = Recorder::new();

    let started = Instant::now();
    let summary = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            recorder.cancel.store(true, Ordering::SeqCst);
        });
        execute("backend-run-1", &chapter_id, &entries, &mut Stub::new(1, 0), Engine::Flux, &recorder.cancel, &recorder.sink())
    });
    let waited = started.elapsed();
    signal(&signals, "release");
    assert!(child.wait().unwrap().success(), "the second process failed");

    assert_eq!(summary.reason, "cancelled");
    assert_eq!(summary.next_page_index, Some(0));
    assert_eq!(summary.errored, 0, "the cancel was counted as a busy chapter");
    assert!(waited < RUN_PATIENCE, "the cancel did not end the wait: {waited:?}");
}

const SIMULTANEOUS_COMMITS: u32 = 15;

/// How long a writer of the simultaneous test waits for the job. The lock
/// polls and keeps no queue, so two threads of one process can hand it back
/// and forth while another process waits. On a slow Windows runner, where a
/// commit took over half a second, that outlasted a command's five seconds and
/// both child writers gave up with `Busy`. The test is about no commit being
/// lost, not about how long a writer waits, so this covers every other
/// writer's commits several times over.
const WRITER_PATIENCE: Duration = Duration::from_secs(120);

fn lock_writer(path: &Path) -> JobLock {
    cleaner_core::project::lock::lock_until(path, WRITER_PATIENCE, &|| false).unwrap()
}

fn patch_ids(path: &Path) -> Vec<String> {
    Job::open(path).unwrap().project.patches.iter().map(|record| record.id.clone()).collect()
}

/// The incident, in two processes: a long edit in one and a commit in the
/// other, each under what its own process calls the job's lock.
#[test]
fn two_processes_cannot_lose_either_committed_edit() {
    let scratch = Scratch::new("two-process-edit");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let path = library.resolve_chapter(&chapter_id).unwrap();
    let signals = scratch.join("signals");
    std::fs::create_dir_all(&signals).unwrap();

    let mut child = spawn_child("slow-edit", &path, &signals, "");
    assert!(wait_for_signal(&signals, "opened", Duration::from_secs(30)), "the second process never read the job");
    {
        let _lock = lock_job(&path).unwrap();
        let mut job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch("runner-attach", 2, Engine::Flux), None).unwrap();
    }
    signal(&signals, "parent-committed");
    assert!(child.wait().unwrap().success(), "the second process failed");

    let ids = patch_ids(&path);
    assert!(ids.contains(&"runner-attach".to_owned()), "the first process's commit was lost: {ids:?}");
    assert!(ids.contains(&"gui-edit".to_owned()), "the second process's commit was lost: {ids:?}");
}

/// A writer that skips the lock altogether cannot write its stale manifest
/// over a commit made after it read: the save is refused and the commit stays.
#[test]
fn a_writer_that_skips_the_lock_cannot_overwrite_a_newer_commit() {
    let scratch = Scratch::new("two-process-unlocked");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let path = library.resolve_chapter(&chapter_id).unwrap();
    let signals = scratch.join("signals");
    std::fs::create_dir_all(&signals).unwrap();

    let mut child = spawn_child("unlocked-edit", &path, &signals, "");
    assert!(wait_for_signal(&signals, "opened", Duration::from_secs(30)), "the second process never read the job");
    {
        let _lock = lock_job(&path).unwrap();
        let mut job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch("runner-attach", 2, Engine::Flux), None).unwrap();
    }
    signal(&signals, "parent-committed");
    assert!(child.wait().unwrap().success(), "the second process failed");

    assert!(signals.join("refused").exists(), "the stale save was not refused");
    let ids = patch_ids(&path);
    assert_eq!(ids, vec!["runner-attach".to_owned()], "the committed edit was overwritten");
}

/// Two processes and two threads committing at once: every commit is there.
#[test]
fn simultaneous_writers_in_two_processes_keep_every_commit() {
    let scratch = Scratch::new("two-process-simultaneous");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let path = library.resolve_chapter(&chapter_id).unwrap();
    let signals = scratch.join("signals");
    std::fs::create_dir_all(&signals).unwrap();

    let mut children: Vec<_> = ["a", "b"].iter()
        .map(|prefix| spawn_child("writer", &path, &signals, prefix))
        .collect();
    // Long enough for both to be waiting on the start, so all four begin together.
    std::thread::sleep(Duration::from_millis(1500));
    signal(&signals, "go");
    let threads: Vec<_> = ["c", "d"].iter().map(|prefix| {
        let path = path.clone();
        let prefix = prefix.to_string();
        std::thread::spawn(move || {
            for n in 0..SIMULTANEOUS_COMMITS {
                let _lock = lock_writer(&path);
                let mut job = Job::open(&path).unwrap();
                job.complete_region(0, &a_patch(&format!("{prefix}-{n}"), n, Engine::Fill), None).unwrap();
            }
        })
    }).collect();
    for thread in threads {
        thread.join().unwrap();
    }
    for child in &mut children {
        assert!(child.wait().unwrap().success(), "a writer process failed");
    }

    let ids = patch_ids(&path);
    let mut missing = Vec::new();
    for prefix in ["a", "b", "c", "d"] {
        for n in 0..SIMULTANEOUS_COMMITS {
            let id = format!("{prefix}-{n}");
            if !ids.contains(&id) {
                missing.push(id);
            }
        }
    }
    assert!(missing.is_empty(), "commits lost: {missing:?}");
}

/// A reader in another process must not sweep a writer's detection masks as
/// orphans in the moment between their write and the manifest that names them.
#[test]
fn a_reader_in_another_process_does_not_sweep_unflushed_detection_masks() {
    let scratch = Scratch::new("two-process-sweep");
    let (library, _, chapter_id) = a_chapter(&scratch, 1);
    let path = library.resolve_chapter(&chapter_id).unwrap();
    let signals = scratch.join("signals");
    std::fs::create_dir_all(&signals).unwrap();

    let mut child = spawn_child("unflushed-detection", &path, &signals, "");
    assert!(wait_for_signal(&signals, "written", Duration::from_secs(30)), "the second process never wrote");
    let _reader = Job::open(&path).unwrap();
    let detections = cleaner_core::project::sidecar_dir(&path).join("detections");
    let survived = detections.join("pending-d9.mask").exists() && detections.join("pending-d9.ink").exists();
    signal(&signals, "parent-read");
    assert!(child.wait().unwrap().success(), "the second process failed");
    assert!(survived, "a reader swept another process's unflushed detection masks");
}

/* ------------------------------------------------------------------ */
/* Honest counters                                                     */
/* ------------------------------------------------------------------ */

/// `counters` is the basis of honest statistics. A cancel-and-resume must
/// therefore end on the same numbers an uninterrupted run does: the resumed
/// page's records are dropped and written again, and a counter that only
/// counted the writing climbed on every cycle.
#[test]
fn a_cancel_and_resume_ends_on_the_same_counters_as_an_uninterrupted_run() {
    let straight = Scratch::new("counters-straight");
    let (library, _, chapter_id) = a_chapter(&straight, 3);
    let recorder = Recorder::new();
    execute(
        "backend-run-1",
        &chapter_id,
        &plan(&library, "chapter", &chapter_id, None).unwrap(),
        &mut Stub::held_back_first(2, 2, 1),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );
    let uninterrupted = Job::open(&library.resolve_chapter(&chapter_id).unwrap())
        .unwrap()
        .project
        .counters;
    assert_eq!(uninterrupted.gate_dropped, 6);
    assert_eq!(uninterrupted.declined, 3);
    // A declined region keeps the lettering its fit started from, for a clean
    // the user asks for, and is not taken for a held candidate.
    let job = Job::open(&library.resolve_chapter(&chapter_id).unwrap()).unwrap();
    let declined: Vec<&RegionUntouched> = job.project.regions_untouched.iter()
        .filter(|row| row.reason == "decline.reason.histogram").collect();
    assert_eq!(declined.len(), 3);
    for row in declined {
        assert_eq!(row.inside_bubble, None);
        let (lettering, scale) = job.load_held_lettering(row).unwrap().expect("a declined region kept its lettering");
        assert_eq!((lettering.count(), scale), (4, 0.5));
    }

    // The same work, cancelled inside page 0 - after its held-back regions were
    // written down and before the page was finished - and then resumed.
    let interrupted = Scratch::new("counters-resumed");
    let (library, _, chapter_id) = a_chapter(&interrupted, 3);
    let first = Recorder::tripping_on("region-done");
    let summary = execute(
        "backend-run-1",
        &chapter_id,
        &plan(&library, "chapter", &chapter_id, None).unwrap(),
        &mut Stub::held_back_first(2, 2, 1),
        Engine::Flux,
        &first.cancel,
        &first.sink(),
    );
    assert_eq!(summary.next_page_index, Some(0), "the cancel did not land inside page 0");

    let resumed = plan(&library, "chapter", &chapter_id, None).unwrap();
    assert_eq!(resumed.first().map(|entry| entry.page_index), Some(0), "page 0 was not redone");
    let second = Recorder::new();
    execute(
        "backend-run-2",
        &chapter_id,
        &resumed,
        &mut Stub::held_back_first(2, 2, 1),
        Engine::Flux,
        &second.cancel,
        &second.sink(),
    );

    let job = Job::open(&library.resolve_chapter(&chapter_id).unwrap()).unwrap();
    assert_eq!(job.project.counters, uninterrupted, "a resumed run counted page 0 twice");
    assert_eq!(job.project.regions_untouched.len(), 9, "the rows were doubled too");
    assert_eq!(job.project.patches.len(), 6);
}

/// The other half: a page that fails is counted once however many runs try it.
/// It is deliberately never marked examined, so every run picks it up again -
/// and every run used to add to `counters.errored` for the same corrupt file.
#[test]
fn a_page_that_fails_on_every_run_is_counted_once() {
    let scratch = Scratch::new("errored-twice");
    let (library, _, chapter_id) = a_chapter(&scratch, 2);
    let mut stub = Stub::new(1, 0);
    stub.fails = vec![library::page_id(&chapter_id, 0)];

    for run in 1..=3 {
        let recorder = Recorder::new();
        let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
        let summary = execute(
            &format!("backend-run-{run}"),
            &chapter_id,
            &entries,
            &mut stub,
            Engine::Flux,
            &recorder.cancel,
            &recorder.sink(),
        );
        assert_eq!(summary.errored, 1, "run {run} reported the wrong number of failures");
        let job = Job::open(&entries[0].job_path).unwrap();
        assert_eq!(
            job.project.counters.errored, 1,
            "the manifest counted one corrupt page {run} times"
        );
        assert_eq!(job.project.errored, vec![0]);
    }

    // And it goes back down when the page succeeds.
    stub.fails.clear();
    let recorder = Recorder::new();
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    execute(
        "backend-run-4",
        &chapter_id,
        &entries,
        &mut stub,
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );
    let job = Job::open(&entries[0].job_path).unwrap();
    assert_eq!(job.project.counters.errored, 0, "a page that was fixed still counts as errored");
    assert!(job.project.errored.is_empty());
}

/* ------------------------------------------------------------------ */
/* A region that cannot be written                                     */
/* ------------------------------------------------------------------ */

/// A full disk, in the shape it actually takes: every region of every page
/// fails to write. The failure used to be swallowed - no event, no counter -
/// and the page was marked examined anyway, so the regions were lost for good
/// and **the chapter reported that no text was detected**, which is the
/// field saying the opposite of the truth.
#[test]
fn a_region_that_cannot_be_written_leaves_the_page_queued_and_the_chapter_honest() {
    let scratch = Scratch::new("full-disk");
    let (library, _, chapter_id) = a_chapter(&scratch, 2);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    // The sidecar directory is where a region's mask and pixels go. Without it
    // every `complete_region` fails at its first write, one region at a time,
    // which is what a disk with nothing left on it does.
    let sidecar = PathBuf::from(format!("{}.d", entries[0].job_path.display()));
    std::fs::remove_dir_all(&sidecar).unwrap();

    let recorder = Recorder::new();
    let summary = execute(
        "backend-run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(2, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    assert_eq!(summary.errored, 2, "a region that could not be written was not counted");
    assert_eq!(summary.pages_cleaned, 0);
    assert_eq!(summary.regions_cleaned, 0);
    // Every page still opens and closes: the `●` comes off even here.
    assert_eq!(recorder.kinds().iter().filter(|kind| **kind == "page-started").count(), 2);
    assert_eq!(recorder.kinds().iter().filter(|kind| **kind == "page-done").count(), 2);

    let job = Job::open(&entries[0].job_path).unwrap();
    assert!(job.project.examined.is_empty(), "a page whose regions were lost was marked examined");
    assert_eq!(job.project.counters.errored, 2);

    let chapter = library
        .list_projects()
        .unwrap()
        .into_iter()
        .flat_map(|project| project.chapters)
        .find(|chapter| chapter.id == chapter_id)
        .unwrap();
    assert!(
        !chapter.no_text_detected,
        "a chapter whose every region failed to write reported that it had no text in it"
    );

    // And the work is still there to be done when there is somewhere to put it.
    assert_eq!(
        plan(&library, "chapter", &chapter_id, None)
            .unwrap()
            .iter()
            .map(|entry| entry.page_index)
            .collect::<Vec<_>>(),
        vec![0, 1],
    );
}

/* ------------------------------------------------------------------ */
/* Events with what is known                                           */
/* ------------------------------------------------------------------ */

/// A `page-started` with no `page-done` leaves the page marked as cleaning for
/// the rest of the session. So a page the manifest can no longer describe is
/// reported with what the run itself knows, rather than not reported at all.
#[test]
fn a_page_the_manifest_cannot_describe_is_still_finished() {
    let scratch = Scratch::new("no-source");
    let (library, _, chapter_id) = a_chapter(&scratch, 2);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    // A manifest that lists the pages and no longer knows what is under them.
    let mut job = Job::open(&entries[0].job_path).unwrap();
    job.project.sources.clear();
    job.flush().unwrap();

    let recorder = Recorder::new();
    execute(
        "backend-run-1",
        &chapter_id,
        &entries,
        &mut Stub::new(1, 0),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );
    assert_eq!(
        recorder.kinds(),
        vec!["page-started", "page-done", "page-started", "page-done", "run-finished"],
        "a page that was started was never finished"
    );
    let page = match recorder.events().into_iter().find(|event| kind_of(event) == "page-done") {
        Some(Event::PageDone { page, .. }) => *page,
        _ => panic!("no page-done"),
    };
    assert_eq!(page.id, library::page_id(&chapter_id, 0));
    assert_eq!(page.index, 0);
    assert_eq!(page.number, 1);
    assert!(page.regions.is_empty());
}

/// `run-finished.regionsCleaned` counts the regions the interface was told
/// about, and not one more: the counter is incremented beside the event, and
/// the event goes out with what is known rather than being dropped.
#[test]
fn every_counted_region_reached_the_stream() {
    let scratch = Scratch::new("counted-regions");
    let (library, _, chapter_id) = a_chapter(&scratch, 3);
    let recorder = Recorder::new();
    let summary = execute(
        "backend-run-1",
        &chapter_id,
        &plan(&library, "chapter", &chapter_id, None).unwrap(),
        &mut Stub::new(2, 1),
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );
    let reported = recorder.kinds().iter().filter(|kind| **kind == "region-done").count();
    assert_eq!(summary.regions_cleaned as usize, reported);
    assert_eq!(reported, 6);
}

/* ------------------------------------------------------------------ */
/* The commands                                                        */
/* ------------------------------------------------------------------ */

/// A cancel that arrives after the run has already finished names nothing. The
/// window is real - `run-finished` has gone out saying `completed` and the slot
/// is not cleared until the worker's release runs - and answering with the run
/// id in it tells a caller its cancel took, a moment after the event stream
/// told it the run completed.
#[test]
fn a_cancel_that_arrives_after_the_run_completed_names_nothing() {
    let _exclusive = exclusively();
    let slot = |reason: Option<&'static str>| a_slot("backend-run-7", "chapter-7", reason);

    *active().lock().unwrap() = vec![slot(Some("completed"))];
    assert_eq!(request_cancel(None).unwrap(), None, "a completed run was reported as cancelled");
    assert_eq!(request_cancel(Some("backend-run-7".into())).unwrap(), None);

    // A run that is still going, and one that stopped because of a cancel, are
    // both cancellable and both name themselves.
    *active().lock().unwrap() = vec![slot(None)];
    assert_eq!(request_cancel(None).unwrap(), Some("backend-run-7".to_owned()));
    *active().lock().unwrap() = vec![slot(Some("cancelled"))];
    assert_eq!(request_cancel(None).unwrap(), Some("backend-run-7".to_owned()));

    // Somebody else's run id is not this one, whatever state it is in.
    assert_eq!(request_cancel(Some("run-7".into())).unwrap(), None);
    active().lock().unwrap().clear();
    assert_eq!(request_cancel(None).unwrap(), None);
}

/// A slot as a run holds it, already finished (so a cancel does not wait), on
/// one chapter.
fn a_slot(run_id: &str, chapter_id: &str, reason: Option<&'static str>) -> Active {
    Active {
        run_id: run_id.to_owned(),
        chapter_id: chapter_id.to_owned(),
        chapters: vec![chapter_id.to_owned()],
        kind: RunKind::Clean,
        progress: Arc::new(RunProgress::default()),
        cancel: Arc::new(AtomicBool::new(false)),
        finished: Arc::new((Mutex::new(true), Condvar::new())),
        outcome: Arc::new(Mutex::new(reason)),
    }
}

/// With two runs in flight, a cancel names the one it stops, and touches
/// neither the other's flag nor its slot. A cancel that names none cannot
/// tell which was meant, and stops nothing.
#[test]
fn a_cancel_reaches_only_the_run_it_names() {
    let _exclusive = exclusively();
    let first = a_slot("backend-run-71", "chapter-a", None);
    let second = a_slot("backend-run-72", "chapter-b", None);
    let (first_flag, second_flag) = (Arc::clone(&first.cancel), Arc::clone(&second.cancel));
    *active().lock().unwrap() = vec![first, second];

    assert_eq!(request_cancel(None).unwrap(), None, "a cancel without an id picked one of two runs");
    assert!(!first_flag.load(Ordering::SeqCst) && !second_flag.load(Ordering::SeqCst));

    assert_eq!(request_cancel(Some("backend-run-72".into())).unwrap(), Some("backend-run-72".to_owned()));
    assert!(second_flag.load(Ordering::SeqCst));
    assert!(!first_flag.load(Ordering::SeqCst), "the other run was cancelled too");
    assert!(is_running("backend-run-71"));
    active().lock().unwrap().clear();
}

/// The ids the backend mints cannot be the mock's. `subscribe` is a merge of
/// both streams, `applyTool({tool: 'autoClean'})` is still the mock's and starts
/// a real mock run, and `editor.run` holds exactly one run id - so two live
/// schedulers minting `run-1` each would be one run as far as the interface is
/// concerned.
#[test]
fn a_run_id_cannot_collide_with_the_mocks() {
    let first = next_run_id();
    let second = next_run_id();
    assert!(first.starts_with("backend-run-"), "{first} could be the mock's");
    assert!(second.starts_with("backend-run-"));
    assert_ne!(first, second);
}


/// The same check `library.rs` makes, for the same reason: a command without
/// `async` is expanded through `body_blocking` and runs *inline* on the invoke
/// handler's thread. A run that opened three ONNX sessions there would hold the
/// webview for the whole of it.
#[test]
fn every_command_is_async_and_so_never_runs_on_the_invoke_handlers_thread() {
    #[allow(clippy::type_complexity)]
    fn check<A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, S, T, R: std::future::Future>(_: fn(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, S, T) -> R) {}
    fn check1<A, R: std::future::Future>(_: fn(A) -> R) {}
    fn check3<A, B, C, R: std::future::Future>(_: fn(A, B, C) -> R) {}
    check(run_clean);
    check1(cancel_run);
    check3(resume_job);
}

#[test]
fn detector_combinations_select_only_their_models() {
    let cases = [
        (serde_json::json!(["ctd"]), vec![DETECTOR]),
        (serde_json::json!(["rtSmall"]), vec![BALLOONS]),
        (serde_json::json!(["rtFull"]), vec![]),
        (serde_json::json!(["samTs"]), vec![]),
        (serde_json::json!(["ctd", "rtFull", "samTs"]), vec![DETECTOR]),
    ];
    for (choice, files) in cases {
        let selected = DetectorModels::from_args(Some(&choice)).unwrap();
        assert_eq!(selected.required(true), files);
        assert_eq!(selected.required(false).last(), Some(&GATE_LABELS));
    }
    assert!(DetectorModels::from_args(Some(&serde_json::json!([]))).is_err());
    assert!(DetectorModels::from_args(Some(&serde_json::json!(["rtSmall", "rtFull"]))).is_err());
    assert!(DetectorModels::from_args(Some(&serde_json::json!(["unknown"]))).is_err());
    let empty = Scratch::new("sam-only-no-legacy-weights");
    let sam = DetectorModels::from_args(Some(&serde_json::json!(["samTs"]))).unwrap();
    assert!(Pipeline::open_selected(&empty.join("."), Preference::CpuOnly, sam, true, None).is_ok());
    assert!(Pipeline::open_selected(&empty.join("."), Preference::CpuOnly, sam, false, None).is_err());
}

/// The cloud GPU's combination parses as one selection: CTD, Full and
/// SAM-TS-L together. Both Ogkalu profiles at once never do.
#[test]
fn the_cloud_combination_parses_and_both_profiles_never_do() {
    let best = DetectorModels::from_args(Some(&serde_json::json!(["ctd", "rtFull", "samTs"]))).unwrap();
    assert_eq!(best, DetectorModels { ctd: true, rt_small: false, rt_full: true, sam: true });
    let both = DetectorModels::from_args(Some(&serde_json::json!(["rtSmall", "rtFull"]))).unwrap_err();
    assert!(both.contains("Full or Small"), "{both}");
    assert!(!both.contains("RT-DETR"), "{both}");
}

#[test]
fn native_run_language_selection_and_optional_reader_are_explicit() {
    let selection = RunSelection::from_args(
        Some(&serde_json::json!({"ja": null, "zh": "ctd-rtdetr", "ko": null})),
        None,
    ).unwrap();
    assert!(!selection.ja && selection.zh && !selection.ko);
    assert!(!selection.ocr_rescue);
    assert!(!selection.allows(&Verdict::Clean { script: "Japanese".into() }));
    assert!(selection.allows(&Verdict::Clean { script: "HanS".into() }));
    assert!(!selection.allows(&Verdict::Clean { script: "Hangul".into() }));
    assert!(!selection.allows(&Verdict::OptedIn),
        "outside-bubble opt-in cannot classify a region against skipped languages");
    assert!(RunSelection::default().allows(&Verdict::OptedIn));
    let skipped = RunSelection::from_args(
        Some(&serde_json::json!({"ja": null, "zh": null, "ko": null})), None,
    ).unwrap();
    assert!(!skipped.any());
    let rescue = RunSelection::from_args(
        Some(&serde_json::json!({"ja": "ctd-rtdetr-ocr"})), None,
    ).unwrap();
    assert!(rescue.ocr_rescue);
    assert!(!RunSelection::from_args(
        Some(&serde_json::json!({"ja": "ctd-rtdetr-ocr"})), Some(false),
    ).unwrap().ocr_rescue);
    assert!(RunSelection::from_args(
        Some(&serde_json::json!({"ko": "unknown"})), None,
    ).is_err());
}

#[test]
fn interrupted_run_restores_its_captured_policy_instead_of_current_defaults() {
    let saved = serde_json::json!({
        "runGeometryPolicy": "legacy",
        "runDetection": {"ja": null, "zh": null, "ko": "ctd-rtdetr"},
        "runOcrRescue": false,
        "runBubbleEngine": "denoise",
        "runOutsideEngine": "fill",
        "runOutsideBubbles": "clean",
        "runBubbleColor": "#102030",
    });
    let resumed = captured_run_policy(&saved).unwrap();
    assert!(!resumed.selection.ja && !resumed.selection.zh && resumed.selection.ko);
    // A run saved with the removed Denoise fill resumes on the fill.
    assert_eq!(resumed.picks.bubble, EnginePick::Fill);
    assert_eq!(resumed.picks.outside, EnginePick::Fill);
    assert_eq!(resumed.outside, OutsideText::Clean);
    assert_eq!(resumed.bubble_color, Some([0x10, 0x20, 0x30]));
    assert!(!resumed.selection.allows(&Verdict::OptedIn), "partial-language outside text is held");
    assert!(captured_run_policy(&serde_json::json!({"runGeometryPolicy": "future"})).is_err());
}

fn snapshot_pipeline(name: &str) -> (Scratch, Pipeline) {
    let scratch = Scratch::new(name);
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for model in REQUIRED_MODELS { std::fs::write(models.join(model), b"").unwrap(); }
    let pipeline = Pipeline::open(&models, Preference::Automatic).unwrap();
    (scratch, pipeline)
}

#[test]
fn interrupted_run_captures_explicit_engine_ceiling() {
    let (_scratch, pipeline) = snapshot_pipeline("ceiling-snapshot");
    let snapshot = run_settings_snapshot(
        &serde_json::json!({"engineCeiling": "lama"}), None, &pipeline,
        Some("fill"), "chapter", None, RunMode::Auto,
    );
    assert_eq!(snapshot["runEngineCeiling"], "fill");
    assert_eq!(captured_run_policy(&snapshot).unwrap().engine_ceiling.as_deref(), Some("fill"));
}

#[test]
fn interrupted_run_restores_captured_global_ceiling_when_run_ceiling_is_null() {
    let (_scratch, pipeline) = snapshot_pipeline("global-ceiling-snapshot");
    let snapshot = run_settings_snapshot(
        &serde_json::json!({"engineCeiling": "fill"}), None, &pipeline,
        None, "chapter", None, RunMode::Auto,
    );
    assert!(snapshot["runEngineCeiling"].is_null());
    let resumed = captured_run_policy(&snapshot).unwrap();
    assert_eq!(resumed.engine_ceiling.as_deref(), Some("fill"));
    assert_eq!(effective_ceiling(resumed.engine_ceiling.as_deref(), Some("lama")), Engine::Fill);

    let missing_run_ceiling = serde_json::json!({"engineCeiling": "fill"});
    assert_eq!(captured_run_policy(&missing_run_ceiling).unwrap().engine_ceiling.as_deref(), Some("fill"));

    let old = captured_run_policy(&serde_json::json!({})).unwrap();
    assert_eq!(old.engine_ceiling, None);
}

/// The switch asks for a reader in every language and under either policy;
/// manga-ocr, which reads only Japanese, still takes it only with Japanese.
/// The reader honours its own provider choice, refuses one it cannot run on,
/// and quietly takes Automatic when it only inherited an unfit whole-app one.
#[test]
fn the_reader_takes_its_own_provider_and_never_inherits_an_unfit_one() {
    use cleaner_core::runtime::package::{Arch, Os, Platform};
    let mac = Some(Platform { os: Os::MacOs, arch: Arch::Aarch64 });
    let linux = Some(Platform { os: Os::Linux, arch: Arch::X86_64 });
    let whole_app_webgpu = serde_json::json!({ "accelerator": "webgpu" });
    assert_eq!(reader_preference(&whole_app_webgpu, mac).unwrap(), Preference::Automatic);
    let own_webgpu = serde_json::json!({ "modelAccelerators": { "hayai": "webgpu" } });
    assert!(reader_preference(&own_webgpu, mac).is_err());
    let own_cuda = serde_json::json!({ "modelAccelerators": { "hayai": "cuda" } });
    assert_eq!(reader_preference(&own_cuda, linux).unwrap(), Preference::Force(cleaner_core::accel::Accelerator::Cuda));
    assert!(reader_preference(&own_cuda, mac).is_err());
    let own_cpu = serde_json::json!({ "modelAccelerators": { "hayai": "cpu" } });
    assert_eq!(reader_preference(&own_cpu, mac).unwrap(), Preference::CpuOnly);
}

#[test]
fn the_reader_switch_reaches_every_language_and_both_policies() {
    let zh_only = serde_json::json!({"ja": null, "zh": "ctd-rtdetr", "ko": null});
    let selection = RunSelection::from_args(Some(&zh_only), Some(true)).unwrap();
    assert!(selection.reader && !selection.ocr_rescue);
    assert!(RunSelection::all_text(Some(true)).reader);
    assert!(!RunSelection::all_text(None).reader);
    let all = RunSelection::default();
    assert!(all.allows(&Verdict::Clean { script: "Hangul_ocr".into() }));
    assert!(all.allows(&Verdict::Clean { script: "Han_ocr".into() }));
    assert!(!selection.allows(&Verdict::Clean { script: "Hangul_ocr".into() }));
    assert!(!all.allows(&Verdict::NotText));
}

#[test]
fn interrupted_run_captures_effective_ocr_rescue_choice() {
    let (_scratch, pipeline) = snapshot_pipeline("effective-ocr-snapshot");
    let requested = RunSelection::from_args(None, Some(true)).unwrap();
    let pipeline = pipeline.with_selection(requested);
    assert!(!pipeline.selection.ocr_rescue);
    let snapshot = run_settings_snapshot(
        &serde_json::json!({}), None, &pipeline, None, "chapter", None, RunMode::Auto,
    );
    assert_eq!(snapshot["runOcrRescue"], false);
}

#[test]
fn default_bubble_fill_clears_stale_captured_color() {
    let (_scratch, pipeline) = snapshot_pipeline("default-bubble-snapshot");
    let snapshot = run_settings_snapshot(
        &serde_json::json!({"runBubbleColor": "#ffffff"}), None, &pipeline,
        None, "chapter", None, RunMode::Auto,
    );
    assert_eq!(captured_run_policy(&snapshot).unwrap().bubble_color, None);
}

/// The settings a run leaves on its job name each detection model by
/// checkpoint, pinned revision, place and spatial input.
#[test]
fn run_record_names_its_exact_detection_models() {
    let (_scratch, mut pipeline) = snapshot_pipeline("models-described-snapshot");
    pipeline.detection_models = DetectorModels { ctd: false, rt_small: false, rt_full: true, sam: true };
    let snapshot = run_settings_snapshot(
        &serde_json::json!({}), None, &pipeline, None, "chapter", None, RunMode::Detect,
    );
    let described = &snapshot["runDetectionModelsDescribed"];
    assert_eq!(described[0]["model"], "samTsL");
    assert_eq!(described[0]["checkpoint"], "mayocream/koharu-text-sam-ts-l");
    assert_eq!(described[0]["revision"], "5dd97423e0fbf2404264979136d47e8101144046");
    assert_eq!(described[0]["execution"], "local");
    assert_eq!(described[0]["spatial"], "whole page (or one long-strip segment crop) in one inference");
    assert_eq!(described[1]["checkpoint"], "ogkalu/comic-text-and-bubble-detector detector.onnx (RT-DETR v2, FP32)");
    assert_eq!(described[1]["revision"], "16e8a622f91fabc6b5b65c96d32d1183f8843546");
    assert_eq!(described[1]["spatial"], "two horizontal halves");
    let text = described.to_string();
    assert!(!text.contains("koharu-layout-rfdetr-seg-2xl-1152") && !text.contains("manga-text-segmentation-2025"));
    // The record still resumes: the description is never read back.
    assert!(captured_run_policy(&snapshot).is_ok());
}

#[test]
fn page_run_captures_scope_for_resume() {
    let (_scratch, pipeline) = snapshot_pipeline("page-scope-snapshot");
    let snapshot = run_settings_snapshot(
        &serde_json::json!({}), None, &pipeline, None, "page", Some(3), RunMode::Auto,
    );
    assert_eq!(snapshot["runScope"], "page");
    assert_eq!(snapshot["runPageIndex"], 3);
    let restored = captured_run_policy(&snapshot).unwrap();
    assert_eq!(restored.scope, "page");
    assert_eq!(restored.page_index, Some(3));
}

#[test]
fn resume_without_a_started_run_does_not_report_success() {
    let empty = RunHandle::default();
    assert!(!resume_started(&empty));
    let active = RunHandle {
        run_id: Some("other".into()), already_running: Some(true), ..RunHandle::default()
    };
    assert!(!resume_started(&active));
}

#[test]
fn skipped_language_and_unclassified_outside_are_counted_as_held() {
    assert!(is_gate_skip("review.reason.languageSkipped"));
    assert!(is_gate_skip("review.reason.outsideLanguageUnverified"));
    assert!(!is_gate_skip("review.reason.declined"));
}

#[test]
fn every_skipped_language_has_its_own_run_notice() {
    let selection = RunSelection::from_args(
        Some(&serde_json::json!({ "ja": null, "zh": null, "ko": null })),
        Some(true),
    ).unwrap();
    assert_eq!(selection.empty_notice(), Some("notice.run.allLanguagesSkipped"));
    assert!(!selection.ocr_rescue);
    assert_eq!(RunSelection::default().empty_notice(), None);
}

#[test]
fn absent_optional_reader_reports_rescue_off_before_cleaning() {
    let scratch = Scratch::new("missing-ocr-notice");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    let notices = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::clone(&notices);
    let subscription = events::register(Box::new(move |event| {
        if let Event::Notice { key, params, .. } = event {
            if key == "notice.run.ocrRescueUnavailable" {
                received.lock().unwrap().push(params.clone());
            }
        }
    }));
    let selected = RunSelection::from_args(None, Some(true)).unwrap();
    let pipeline = Pipeline::open(&models, Preference::Automatic).unwrap().with_selection(selected);
    assert!(!pipeline.selection.ocr_rescue);
    assert_eq!(notices.lock().unwrap().len(), 1);
    assert!(notices.lock().unwrap()[0]["reason"].as_str().unwrap().contains(OCR_ENCODER));

    for name in OCR_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    let pipeline = Pipeline::open(&models, Preference::Automatic).unwrap().with_selection(selected);
    assert!(!pipeline.selection.ocr_rescue);
    assert_eq!(notices.lock().unwrap().len(), 2);
    assert!(notices.lock().unwrap()[1]["reason"].as_str().unwrap().contains("Empty reader file"));
    events::unregister(subscription);
}

/// A reader asked for and not installed is always said: Hayai missing with
/// no manga-ocr fallback asked (no Japanese cleaned, or All text, which is
/// what a cloud run under that policy sends) names the missing Hayai files.
/// With Japanese cleaned, manga-ocr stands in and speaks for itself in
/// `with_selection`, so nothing is said twice. Installed, or not asked,
/// nothing is said.
#[test]
fn a_missing_text_reader_is_never_dropped_quietly() {
    let scratch = Scratch::new("missing-hayai-reason");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    let reason = unavailable_hayai_reason(&models).expect("nothing installed");
    for name in HAYAI_MODELS { assert!(reason.contains(name), "{reason}"); }

    let zh_only = serde_json::json!({"ja": null, "zh": "ctd-rtdetr", "ko": null});
    let no_japanese = RunSelection::from_args(Some(&zh_only), Some(true)).unwrap();
    assert_eq!(reader_gap(no_japanese, unavailable_hayai_reason(&models)).as_deref(), Some(reason.as_str()));
    let all_text = RunSelection::all_text(Some(true));
    assert_eq!(reader_gap(all_text, unavailable_hayai_reason(&models)).as_deref(), Some(reason.as_str()));
    let japanese = RunSelection::from_args(None, Some(true)).unwrap();
    assert_eq!(reader_gap(japanese, unavailable_hayai_reason(&models)), None);
    assert_eq!(reader_gap(RunSelection::all_text(None), unavailable_hayai_reason(&models)), None);

    // One empty file is still missing; all three present is a reader.
    for name in HAYAI_MODELS { std::fs::write(models.join(name), b"x").unwrap(); }
    std::fs::write(models.join(HAYAI_MODELS[1]), b"").unwrap();
    assert!(unavailable_hayai_reason(&models).is_some_and(|reason| reason.contains(HAYAI_MODELS[1])));
    std::fs::write(models.join(HAYAI_MODELS[1]), b"x").unwrap();
    assert_eq!(unavailable_hayai_reason(&models), None);
    assert_eq!(reader_gap(all_text, unavailable_hayai_reason(&models)), None);
}

#[test]
fn skipped_language_is_held_in_the_page_clean_path() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{DetBox, DetectedLanguage, Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, _: usize) -> Detection {
        let boxes = [40, 150].map(|x| DetBox {
            rect: Rect::new(x, 55, 28, 28),
            confidence: 0.95,
            language: DetectedLanguage::Japanese,
        });
        let mut levels = vec![0; (crop.width * crop.height) as usize];
        for x in [40, 150] {
            for y in 55..83 {
                for xx in x..x + 28 {
                    levels[(y * crop.width as i64 + xx) as usize] = 255;
                }
            }
        }
        Detection {
            boxes: boxes.to_vec(),
            segmentation: Segmentation {
                width: crop.width,
                height: crop.height,
                levels,
                fit: Letterbox::fit(crop.width, crop.height),
            },
        }
    }
    fn balloons(_: &Raster, _: usize) -> Vec<BalloonBox> {
        [40, 150].map(|x| BalloonBox {
            rect: Rect::new(x - 15, 40, 65, 60),
            class: BalloonClass::TextInBubble,
            score: 0.9,
        }).to_vec()
    }
    fn judge(region: &Region) -> Verdict {
        let script = if region.masking.x < 100 { "Japanese" } else { "Hangul" };
        Verdict::Clean { script: script.to_owned() }
    }

    let scratch = Scratch::new("selected-page-clean");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    let selection = RunSelection::from_args(
        Some(&serde_json::json!({ "ja": null, "zh": null, "ko": "ctd-rtdetr" })),
        None,
    ).unwrap();
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap()
        .with_selection(selection)
        .with_picks(Picks { bubble: EnginePick::Fill, outside: EnginePick::Fill })
        .with_bubble_color(Some([245, 245, 245]));
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(240, 140);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &survey.segments,
        placement: 0, sources: &[],
    };
    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 2);
    assert!(matches!(&outcome.regions[0], RegionOutcome::Untouched { reason, .. }
        if reason == "review.reason.languageSkipped"));
    assert!(matches!(&outcome.regions[1], RegionOutcome::Cleaned(..)),
        "{}", one_line(&outcome.regions[1]));
    assert_eq!(outcome.regions.iter().filter(|r| matches!(r, RegionOutcome::Cleaned(..))).count(), 1);
    if let RegionOutcome::Cleaned(patch, _) = &outcome.regions[1] {
        assert!(patch.mask.contains(160, 65));
    }
}

fn gray_test_page(width: u32, height: u32) -> Raster {
    Raster {
        width, height, mode: ColorMode::Gray, depth: BitDepth::Eight,
        icc: None, palette: None, trns: None, srgb_intent: None,
        color: Default::default(),
        data: vec![245; (width * height) as usize],
    }
}

#[test]
fn a_box_taller_than_the_strip_overlap_is_held_at_its_full_extent() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, _: usize) -> Detection {
        Detection {
            boxes: Vec::new(),
            segmentation: Segmentation {
                width: crop.width,
                height: crop.height,
                levels: vec![0; (crop.width * crop.height) as usize],
                fit: Letterbox::fit(crop.width, crop.height),
            },
        }
    }
    fn balloons(crop: &Raster, segment: usize) -> Vec<BalloonBox> {
        // The two detector crops see clipped parts of one box at y=1500..3200.
        let rect = match segment {
            0 => {
                assert_eq!((crop.width, crop.height), (400, 2_900));
                Rect::new(80, 1_500, 120, 1_400)
            }
            1 => {
                assert_eq!((crop.width, crop.height), (400, 2_000));
                Rect::new(80, 0, 120, 1_200)
            }
            _ => panic!("unexpected segment {segment}"),
        };
        vec![BalloonBox { rect, class: BalloonClass::TextFree, score: 0.9 }]
    }
    fn judge(_: &Region) -> Verdict { Verdict::Uncertain }

    let scratch = Scratch::new("strip-cut-adoption");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap();
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 4_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let splits = [Split { y: 2_000, kind: SplitKind::Fallback }];
    let segments = cleaner_core::strip::detection_segments(4_000, &splits);
    assert_eq!(segments.len(), 2);
    assert_eq!((segments[0].index, segments[0].start), (0, 0),
        "the held box starts in segment zero");
    assert_eq!((segments[1].index, segments[1].start), (1, 2_000));
    assert_eq!(segments[0].detect_end, 2_900);
    let first_detector_box = Rect::new(80, 1_500, 120, 1_400);
    let second_detector_box_in_strip = Rect::new(80, 2_000, 120, 1_200);
    let full_box = rect_hull(first_detector_box, second_detector_box_in_strip);
    assert_eq!(full_box, Rect::new(80, 1_500, 120, 1_700));
    assert!(full_box.y < segments[1].start as i64);
    assert!(full_box.bottom() > segments[0].detect_end as i64);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 1);
    assert!(matches!(&outcome.regions[0], RegionOutcome::Untouched { bbox, reason }
        if *bbox == Rect::new(73, 1_493, 135, 1_714)
            && reason == "review.reason.gateSkippedLowConfidence"),
        "{}", one_line(&outcome.regions[0]));
}

#[test]
fn a_clipped_chain_without_a_third_fragment_keeps_its_review_row() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, _: usize) -> Detection {
        Detection { boxes: Vec::new(), segmentation: Segmentation {
            width: crop.width, height: crop.height,
            levels: vec![0; (crop.width * crop.height) as usize],
            fit: Letterbox::fit(crop.width, crop.height),
        } }
    }
    fn balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        let rect = match segment {
            0 => Some(Rect::new(80, 1_500, 120, 1_400)),
            1 => Some(Rect::new(80, 0, 120, 2_900)),
            2 => None,
            _ => panic!("unexpected segment {segment}"),
        };
        rect.into_iter().map(|rect| BalloonBox {
            rect, class: BalloonClass::TextFree, score: 0.9,
        }).collect()
    }
    fn judge(_: &Region) -> Verdict { Verdict::Uncertain }

    let scratch = Scratch::new("strip-three-segment-chain");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS { std::fs::write(models.join(name), b"").unwrap(); }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap();
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 6_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let segments = cleaner_core::strip::detection_segments(6_000, &[
        Split { y: 2_000, kind: SplitKind::Fallback },
        Split { y: 4_000, kind: SplitKind::Fallback },
    ]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 1, "three-segment chain lost its review row");
    assert!(matches!(&outcome.regions[0], RegionOutcome::Untouched { bbox, .. }
        if bbox.y <= 1_500 && bbox.bottom() >= 4_900));
}

#[test]
fn tall_clipped_fragments_match_across_the_full_detection_overlap() {
    let upper = Rect::new(80, 900, 120, 2_000);
    let lower = Rect::new(80, 2_000, 120, 2_000);
    assert_eq!(upper.bottom() - lower.y, strip::DETECTION_OVERLAP as i64);
    assert!(clipped_continuation(&upper, &lower, 2_000));
    assert!(!clipped_continuation(&upper, &lower, 1_900));
    assert!(!clipped_continuation(&upper, &Rect::new(140, 2_000, 120, 2_000), 2_000));
}

#[test]
fn distinct_tall_balloons_overlapping_near_the_relaxed_threshold_stay_separate() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, _: usize) -> Detection {
        Detection { boxes: Vec::new(), segmentation: Segmentation {
            width: crop.width, height: crop.height,
            levels: vec![0; (crop.width * crop.height) as usize],
            fit: Letterbox::fit(crop.width, crop.height),
        } }
    }
    fn balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        let rect = match segment {
            0 => Rect::new(80, 900, 120, 2_000),
            1 => Rect::new(140, 225, 120, 2_000),
            _ => panic!("unexpected segment {segment}"),
        };
        vec![BalloonBox { rect, class: BalloonClass::TextFree, score: 0.9 }]
    }
    fn judge(_: &Region) -> Verdict { Verdict::Uncertain }

    let scratch = Scratch::new("strip-distinct-tall-balloons");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS { std::fs::write(models.join(name), b"").unwrap(); }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap();
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 5_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let segments = cleaner_core::strip::detection_segments(5_000,
        &[Split { y: 2_000, kind: SplitKind::Fallback }]);
    assert_eq!(segments[0].detect_end, 2_900);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 2, "distinct balloons were joined: {:?}",
        outcome.regions.iter().map(one_line).collect::<Vec<_>>());
}

#[test]
fn a_join_keeps_the_provisional_index_for_later_patch_ids() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{DetBox, DetectedLanguage, Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, segment: usize) -> Detection {
        let boxes = if segment == 1 {
            vec![DetBox {
                rect: Rect::new(250, 1_350, 32, 32),
                confidence: 0.95,
                language: DetectedLanguage::Japanese,
            }]
        } else { Vec::new() };
        let mut levels = vec![0; (crop.width * crop.height) as usize];
        if segment == 1 {
            for y in 1_350..1_382 {
                for x in 250..282 {
                    levels[(y * crop.width + x) as usize] = 255;
                }
            }
        }
        Detection { boxes, segmentation: Segmentation {
            width: crop.width, height: crop.height, levels,
            fit: Letterbox::fit(crop.width, crop.height),
        } }
    }
    fn balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        let rects = if segment == 0 {
            vec![Rect::new(80, 1_500, 120, 1_400)]
        } else {
            vec![Rect::new(80, 0, 120, 1_200), Rect::new(240, 1_340, 55, 55)]
        };
        rects.into_iter().map(|rect| BalloonBox {
            rect, class: BalloonClass::TextFree, score: 0.9,
        }).collect()
    }
    fn balloons_without_join(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        let rect = if segment == 0 {
            Rect::new(80, 1_500, 120, 1_400)
        } else {
            Rect::new(240, 1_340, 55, 55)
        };
        vec![BalloonBox { rect, class: BalloonClass::TextFree, score: 0.9 }]
    }
    fn judge(_: &Region) -> Verdict { Verdict::Clean { script: "Japanese".into() } }

    let scratch = Scratch::new("strip-join-index");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS { std::fs::write(models.join(name), b"").unwrap(); }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap()
        .with_picks(Picks { bubble: EnginePick::Fill, outside: EnginePick::Fill })
        .with_bubble_color(Some([245, 245, 245]));
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 4_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let segments = cleaner_core::strip::detection_segments(4_000,
        &[Split { y: 2_000, kind: SplitKind::Fallback }]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 2);
    assert!(matches!(&outcome.regions[0], RegionOutcome::Untouched { bbox, reason }
        if *bbox == Rect::new(73, 1_493, 135, 1_714)
            && reason == "review.reason.gateSkippedLowConfidence"));
    let RegionOutcome::Cleaned(patch, _) = &outcome.regions[1] else {
        panic!("later region was not cleaned: {}", one_line(&outcome.regions[1]));
    };
    assert_eq!(patch.id, "c1-p001-r1");
    assert_eq!(patch.order, 1);

    let mut without_join = Pipeline::open(&models, Preference::Automatic).unwrap()
        .with_picks(Picks { bubble: EnginePick::Fill, outside: EnginePick::Fill })
        .with_bubble_color(Some([245, 245, 245]));
    without_join.test_vision = Some(TestVision { detect, balloons: balloons_without_join, judge });
    let baseline = without_join.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    let baseline_patch = baseline.regions.iter().find_map(|region| match region {
        RegionOutcome::Cleaned(patch, _) => Some(patch),
        _ => None,
    }).expect("the later region cleans without a join");
    assert_eq!((patch.id.as_str(), patch.order),
        (baseline_patch.id.as_str(), baseline_patch.order));
}

#[test]
fn an_unowned_clipped_fragment_cannot_withdraw_an_unrelated_patch() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{DetBox, DetectedLanguage, Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, segment: usize) -> Detection {
        let boxes = if segment == 0 {
            vec![DetBox {
                rect: Rect::new(250, 1_800, 32, 32),
                confidence: 0.95,
                language: DetectedLanguage::Japanese,
            }]
        } else { Vec::new() };
        let mut levels = vec![0; (crop.width * crop.height) as usize];
        if segment == 0 {
            for y in 1_800..1_832 {
                for x in 250..282 {
                    levels[(y * crop.width + x) as usize] = 255;
                }
            }
        }
        Detection { boxes, segmentation: Segmentation {
            width: crop.width, height: crop.height, levels,
            fit: Letterbox::fit(crop.width, crop.height),
        } }
    }
    fn balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        let rects = if segment == 0 {
            vec![Rect::new(240, 1_790, 55, 55), Rect::new(80, 2_020, 120, 880)]
        } else {
            vec![Rect::new(80, 20, 120, 1_180)]
        };
        rects.into_iter().map(|rect| BalloonBox {
            rect, class: BalloonClass::TextFree, score: 0.9,
        }).collect()
    }
    fn judge(_: &Region) -> Verdict { Verdict::Clean { script: "Japanese".into() } }

    let scratch = Scratch::new("strip-unowned-fragment");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS { std::fs::write(models.join(name), b"").unwrap(); }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap()
        .with_picks(Picks { bubble: EnginePick::Fill, outside: EnginePick::Fill })
        .with_bubble_color(Some([245, 245, 245]));
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 4_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let segments = cleaner_core::strip::detection_segments(4_000,
        &[Split { y: 2_000, kind: SplitKind::Fallback }]);
    assert!(segments[1].start <= 2_013 && segments[1].end > 2_013);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 2);
    let patch = outcome.regions.iter().find_map(|region| match region {
        RegionOutcome::Cleaned(patch, _) => Some(patch),
        _ => None,
    }).expect("the unrelated box survives the join");
    assert_eq!(patch.id, "c1-p001-r0");
    assert_eq!(patch.order, 0);
    assert!(matches!(&outcome.regions[1], RegionOutcome::Untouched { bbox, reason }
        if *bbox == Rect::new(73, 2_013, 135, 1_194)
            && reason == "review.reason.gateSkippedLowConfidence"));
}

#[test]
fn an_unowned_forward_overlap_fragment_does_not_add_a_review_row() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{DetBox, DetectedLanguage, Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, segment: usize) -> Detection {
        let boxes = if segment == 1 { vec![DetBox {
            rect: Rect::new(160, 500, 28, 28), confidence: 0.95,
            language: DetectedLanguage::Japanese,
        }] } else { Vec::new() };
        let mut levels = vec![0; (crop.width * crop.height) as usize];
        if segment == 1 {
            for y in 500..528 {
                for x in 160..188 { levels[(y * crop.width + x) as usize] = 255; }
            }
        }
        Detection { boxes, segmentation: Segmentation {
            width: crop.width, height: crop.height, levels,
            fit: Letterbox::fit(crop.width, crop.height),
        } }
    }
    fn balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        let rect = if segment == 0 {
            Rect::new(80, 2_020, 120, 880)
        } else {
            Rect::new(150, 20, 120, 1_180)
        };
        vec![BalloonBox { rect, class: BalloonClass::TextFree, score: 0.9 }]
    }
    fn judge(_: &Region) -> Verdict { Verdict::Clean { script: "Japanese".into() } }

    let scratch = Scratch::new("strip-unowned-overlap-row");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS { std::fs::write(models.join(name), b"").unwrap(); }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap()
        .with_picks(Picks { bubble: EnginePick::Fill, outside: EnginePick::Fill })
        .with_bubble_color(Some([245, 245, 245]));
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 4_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let segments = cleaner_core::strip::detection_segments(4_000,
        &[Split { y: 2_000, kind: SplitKind::Fallback }]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 1, "unowned overlap added a review row");
    assert!(matches!(&outcome.regions[0], RegionOutcome::Cleaned(..)));
}

#[test]
fn an_edge_box_without_a_continuation_follows_the_normal_adoption_path() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, _: usize) -> Detection {
        Detection {
            boxes: Vec::new(),
            segmentation: Segmentation {
                width: crop.width,
                height: crop.height,
                levels: vec![0; (crop.width * crop.height) as usize],
                fit: Letterbox::fit(crop.width, crop.height),
            },
        }
    }
    fn balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        if segment == 0 {
            vec![BalloonBox {
                rect: Rect::new(80, 1_500, 120, 1_400),
                class: BalloonClass::TextFree,
                score: 0.9,
            }]
        } else {
            Vec::new()
        }
    }
    fn judge(_: &Region) -> Verdict { Verdict::Clean { script: "Japanese".into() } }

    let scratch = Scratch::new("strip-edge-whole-box");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    let selection = RunSelection::from_args(
        Some(&serde_json::json!({ "ja": null, "zh": null, "ko": "ctd-rtdetr" })), None,
    ).unwrap();
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap()
        .with_selection(selection);
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 4_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let segments = cleaner_core::strip::detection_segments(4_000,
        &[Split { y: 2_000, kind: SplitKind::Fallback }]);
    assert_eq!(segments[0].detect_end, 2_900);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 1);
    assert!(matches!(&outcome.regions[0], RegionOutcome::Untouched { bbox, reason }
        if *bbox == Rect::new(73, 1_493, 135, 1_407)
            && reason == "review.reason.languageSkipped"),
        "an unmatched edge box lost its normal gate result: {}", one_line(&outcome.regions[0]));
}

#[test]
fn two_prior_fragments_matching_one_current_box_keep_both_review_areas() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, _: usize) -> Detection {
        Detection {
            boxes: Vec::new(),
            segmentation: Segmentation {
                width: crop.width,
                height: crop.height,
                levels: vec![0; (crop.width * crop.height) as usize],
                fit: Letterbox::fit(crop.width, crop.height),
            },
        }
    }
    fn balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        let rects = match segment {
            0 => vec![Rect::new(80, 1_500, 100, 1_400), Rect::new(180, 1_700, 100, 1_200)],
            1 => vec![Rect::new(120, 0, 120, 1_200)],
            _ => panic!("unexpected segment {segment}"),
        };
        rects.into_iter().map(|rect| BalloonBox {
            rect, class: BalloonClass::TextFree, score: 0.9,
        }).collect()
    }
    fn judge(_: &Region) -> Verdict { Verdict::Uncertain }

    let scratch = Scratch::new("strip-two-priors-one-current");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap();
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 4_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let segments = cleaner_core::strip::detection_segments(4_000,
        &[Split { y: 2_000, kind: SplitKind::Fallback }]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 2, "one prior fragment was lost: {:?}",
        outcome.regions.iter().map(one_line).collect::<Vec<_>>());
    assert!(outcome.regions.iter().all(|region| matches!(region, RegionOutcome::Untouched { .. })));
    let bounds = outcome.regions.iter().map(|region| match region {
        RegionOutcome::Untouched { bbox, .. } => *bbox,
        _ => unreachable!(),
    }).collect::<Vec<_>>();
    assert!(bounds.iter().any(|bbox| bbox.x <= 80 && bbox.right() >= 240
        && bbox.y <= 1_500 && bbox.bottom() >= 3_200), "joined box missing: {bounds:?}");
    assert!(bounds.iter().any(|bbox| bbox.x <= 180 && bbox.right() >= 280
        && bbox.y <= 1_700 && bbox.bottom() >= 2_900), "unmatched prior missing: {bounds:?}");
}

#[test]
fn distinct_boxes_meeting_at_a_strip_cut_stay_separate() {
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{Detection, Letterbox, Region, Segmentation};

    fn detect(crop: &Raster, _: usize) -> Detection {
        Detection {
            boxes: Vec::new(),
            segmentation: Segmentation {
                width: crop.width,
                height: crop.height,
                levels: vec![0; (crop.width * crop.height) as usize],
                fit: Letterbox::fit(crop.width, crop.height),
            },
        }
    }
    fn balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
        let above = BalloonBox {
            rect: Rect::new(80, 1_600, 120, 400),
            class: BalloonClass::TextFree,
            score: 0.9,
        };
        let below = BalloonBox {
            rect: Rect::new(80, 2_000, 120, 400),
            class: BalloonClass::TextFree,
            score: 0.9,
        };
        match segment {
            0 => vec![above, below],
            1 => vec![BalloonBox { rect: Rect::new(80, 0, 120, 400), ..below }],
            _ => panic!("unexpected segment {segment}"),
        }
    }
    fn judge(_: &Region) -> Verdict { Verdict::Uncertain }

    let scratch = Scratch::new("strip-cut-distinct-boxes");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap();
    pipeline.test_vision = Some(TestVision { detect, balloons, judge });
    let page = gray_test_page(400, 4_000);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let segments = cleaner_core::strip::detection_segments(4_000,
        &[Split { y: 2_000, kind: SplitKind::Fallback }]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &segments,
        placement: 0, sources: &[],
    };

    let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
    assert_eq!(outcome.regions.len(), 2);
    let mut bounds = outcome.regions.iter().map(|region| match region {
        RegionOutcome::Untouched { bbox, .. } => *bbox,
        other => panic!("unexpected outcome: {}", one_line(other)),
    }).collect::<Vec<_>>();
    bounds.sort_by_key(|bbox| bbox.y);
    assert_eq!(bounds, [Rect::new(73, 1_593, 135, 414), Rect::new(73, 1_993, 135, 414)]);
}

/// The seam's own names, on the wire. `runClean` resolves with the queue and
/// `runId` is null when nothing was queued.
#[test]
fn the_run_handle_is_serialised_in_the_names_the_seam_declares() {
    let handle = RunHandle {
        run_id: Some("run-1".into()),
        pages: vec![QueuedPage {
            chapter_id: "c1".into(),
            page_id: "c1-p001".into(),
            page_index: 0,
        }],
        ..RunHandle::default()
    };
    let value = serde_json::to_value(&handle).unwrap();
    assert_eq!(value["runId"], "run-1");
    assert_eq!(value["pages"][0]["chapterId"], "c1");
    assert_eq!(value["pages"][0]["pageId"], "c1-p001");
    assert_eq!(value["pages"][0]["pageIndex"], 0);
    assert!(value.get("alreadyRunning").is_none(), "alreadyRunning was sent when it is not one");

    let nothing =
        serde_json::to_value(RunHandle::default())
            .unwrap();
    assert!(nothing["runId"].is_null());
    assert_eq!(nothing["pages"].as_array().unwrap().len(), 0);
}

#[test]
fn parse_color_hex_parses_valid_hex_and_rejects_invalid() {
    assert_eq!(parse_color_hex(Some("#ffffff")), Some([255, 255, 255]));
    assert_eq!(parse_color_hex(Some("ffffff")), Some([255, 255, 255]));
    assert_eq!(parse_color_hex(Some("#000000")), Some([0, 0, 0]));
    assert_eq!(parse_color_hex(Some("#ff8020")), Some([255, 128, 32]));
    assert_eq!(parse_color_hex(Some("#123")), None);
    assert_eq!(parse_color_hex(Some("invalid")), None);
    assert_eq!(parse_color_hex(Some("ééé")), None);
    assert_eq!(parse_color_hex(None), None);
}

#[test]
fn clean_region_with_solid_color_paints_chosen_color() {
    let page = gray_page(|_, _| 255);
    let fitted = fitted_over(&page, Rect::new(40, 40, 20, 20));
    let mut rung2 = no_rung_two();
    let attempt = clean_region_with_color(
        &mut rung2,
        &page,
        &fitted,
        Engine::Fill,
        Some(EnginePick::Fill),
        fit::page_noise_sigma(&page),
        // Deliberately unlike the white surrounding paper: an explicit solid
        // colour must not be rejected and escalated away by quality scoring.
        Some([0, 0, 0]),
    )
    .expect("clean_region_with_color");
    match attempt {
        Attempt::Cleaned(made, _) => {
            assert_eq!(made.engine, Engine::Fill);
            let bounds = made.mask.bounds;
            let painted = (bounds.y..bounds.bottom())
                .find_map(|y| {
                    (bounds.x..bounds.right())
                        .find(|&x| made.mask.contains(x, y))
                        .map(|x| (x, y))
                })
                .expect("the fitted mask contains a pixel");
            assert_eq!(
                made.pixels.sample(
                    (painted.0 - bounds.x) as u32,
                    (painted.1 - bounds.y) as u32,
                    0,
                ),
                0,
            );
        }
        Attempt::Declined(reason) => panic!("clean_region_with_color was declined: {reason}"),
    }
}

/// The model search order, which is the one thing about the weights this
/// machine can check without them: an override first, then the per-user
/// directory a first-launch download writes to, then the installed copy.
#[test]
fn the_weights_are_looked_for_where_they_are_put() {
    let paths = model_search_paths(Some(Path::new("/data")));
    assert!(paths.contains(&PathBuf::from("/data/models")));
    assert_eq!(paths.last(), Some(&PathBuf::from("models")));
}

/// The dylib `scripts/fetch-runtime.sh` unpacks into the checkout's
/// `runtimes/<package>/lib/`.
fn fetched_runtime() -> Option<PathBuf> {
    let name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    std::fs::read_dir("../runtimes")
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("lib").join(name))
        .find(|path| path.exists())
}

/// The real pipeline, over the real weights, on one page - **when they are
/// there**.
///
/// It skips rather than fails when they are not, and that is deliberate: the
/// weights are not in the repository and the ONNX Runtime is fetched rather
/// than bundled, so a machine that has run neither `scripts/fetch-models.sh`
/// nor `scripts/fetch-runtime.sh` cannot run this and should not be told it
/// has broken something.
///
/// What it establishes is narrow and worth having: that the scheduler's
/// [`Cleaner`] is satisfied by the same sequence `spikes/clean-page` runs, that
/// a region reaches a patch with a well-formed pixel buffer, and that the
/// provenance a run writes is the provenance the seam reads back. It is **not**
/// a throughput measurement and not a flag-rate one - Phase 1's exit criteria
/// need a licence-cleared corpus, and `fixtures/pages/` is synthetic.
/// The real pipeline, or `None` when this machine cannot build one.
///
/// It skips rather than fails, and that is deliberate: the weights are not in
/// the repository and the ONNX Runtime is fetched rather than bundled, so a
/// machine that has run neither `scripts/fetch-models.sh` nor
/// `scripts/fetch-runtime.sh` cannot run these and should not be told it has
/// broken something.
///
/// **One at a time.** Every test that builds one of these holds
/// [`one_gpu_at_a_time`] for as long as it uses it. Rung 2 lands on WebGPU on
/// this machine, `cargo test` runs its tests on as many threads as
/// the machine has cores, and two Metal command buffers encoded from two
/// threads through one process abort the whole binary - *"A command encoder is
/// already encoding to this command buffer"*, `SIGABRT`, every test in the
/// binary lost. That is a property of driving one GPU from several threads and
/// not of the pipeline: the application holds one [`Pipeline`] per run and has
/// no path that opens two.
fn the_real_pipeline() -> Option<Pipeline> {
    let models = model_dir(None).or_else(|| {
        let up = PathBuf::from("../models");
        models_ready(&up).then_some(up)
    });
    let Some(models) = models else {
        eprintln!("skipped: no model weights - run scripts/fetch-models.sh");
        return None;
    };
    // `runtime::find` looks where an *installed* application would; a test
    // binary sits in `target/debug/deps`, where nothing was ever unpacked. So
    // the checkout's own `runtimes/` is searched too, and these run on a
    // machine that has fetched the runtime without needing `ORT_DYLIB_PATH` in
    // the environment - which the runtime module's own tests assert is unset.
    let Some(runtime) = cleaner_core::runtime::find(None).ok().or_else(fetched_runtime) else {
        eprintln!("skipped: no ONNX Runtime - run scripts/fetch-runtime.sh");
        return None;
    };
    if cleaner_core::runtime::load(&runtime).is_err() {
        eprintln!("skipped: the ONNX Runtime would not load");
        return None;
    }
    Pipeline::open(&models, Preference::Automatic).ok()
}

/// The lock every real-pipeline test takes, for the reason
/// [`the_real_pipeline`] gives. Poisoning is ignored: a panicking test has
/// already failed, and taking the rest of the binary down with it turns one
/// red line into a hundred.
/// `open_gate` with the reader's three files absent, and with them present but
/// unreadable. Both open a gate, and neither has a reader - the second is the
/// fallback for a reader that will not open, which had no test.
///
/// Skips like [`the_real_pipeline`] and for the same reason: the gate's own
/// weights are needed to open it at all.
#[test]
fn a_reader_that_is_absent_or_will_not_open_leaves_the_gate_standing() {
    let _gpu = one_gpu_at_a_time();
    let Some(pipeline_models) = model_dir(None).or_else(|| {
        let up = PathBuf::from("../models");
        models_ready(&up).then_some(up)
    }) else {
        eprintln!("skipped: no model weights - run scripts/fetch-models.sh");
        return;
    };
    let Some(runtime) = cleaner_core::runtime::find(None).ok().or_else(fetched_runtime) else {
        eprintln!("skipped: no ONNX Runtime - run scripts/fetch-runtime.sh");
        return;
    };
    if cleaner_core::runtime::load(&runtime).is_err() {
        eprintln!("skipped: the ONNX Runtime would not load");
        return;
    }

    let scratch = Scratch::new("gate-without-reader");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in [GATE_MODEL, GATE_LABELS] {
        std::fs::copy(pipeline_models.join(name), models.join(name)).unwrap();
    }

    let notices = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::clone(&notices);
    let subscription = events::register(Box::new(move |event| {
        if let Event::Notice { key, params, .. } = event {
            if key == "notice.run.ocrRescueUnavailable" {
                received.lock().unwrap().push(params.clone());
            }
        }
    }));

    let bare = open_gate(&models, Preference::Automatic).expect("the gate opens without a reader");
    assert!(!bare.has_reader(), "no reader files, no reader");
    assert!(notices.lock().unwrap()[0]["reason"].as_str().unwrap().contains(OCR_ENCODER));

    for name in OCR_MODELS {
        std::fs::write(models.join(name), b"not a model").unwrap();
    }
    let fallen_back =
        open_gate(&models, Preference::Automatic).expect("a reader that will not open is not a failure");
    assert!(!fallen_back.has_reader(), "a corrupt reader must fall back to the bare gate");
    let notices = notices.lock().unwrap();
    assert_eq!(notices.len(), 2);
    let reason = notices[1]["reason"].as_str().unwrap();
    assert!(!reason.is_empty() && reason.len() <= 180);
    drop(notices);
    events::unregister(subscription);
}

fn one_gpu_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GPU.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One fixture page through it, with the geometry a run would hand it: one
/// page, its own strip, one segment, no joins. A single-page project is the
/// degenerate case of rules 1 to 4 and it has to come out where it came out
/// before they were wired.
fn clean_fixture(pipeline: &mut Pipeline, fixture: &str, ceiling: Engine) -> Option<PageOutcome> {
    let page = PathBuf::from("../fixtures/pages").join(fixture);
    let Ok(bytes) = std::fs::read(&page) else {
        eprintln!("skipped: no {}", page.display());
        return None;
    };
    let decoded = cleaner_core::image::decode(&bytes).expect("the fixture decoded");
    let strip = Strip::of_sizes(&[(decoded.width, decoded.height)]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let joins = survey.joins.clone();
    let context =
        PageContext { strip: &strip, joins: &joins, segments: &survey.segments, placement: 0, sources: &[] };
    Some(pipeline.clean_page("c1-p001", &bytes, ceiling, &context).expect("the page cleaned"))
}

/// What one region came out as, in one line. The evidence an end-to-end run
/// reports, and the reason these tests are worth running with `--nocapture`.
fn one_line(region: &RegionOutcome) -> String {
    match region {
        RegionOutcome::Cleaned(patch, review) => {
            let provenance = &patch.provenance;
            let params = &provenance.params_snapshot;
            format!(
                "{}: {:?} on {} - {}×{} mask, {} ms, tiles {}, pad {}, quality {}, digest {}{}",
                patch.id,
                provenance.engine,
                provenance.execution_provider,
                patch.mask.bounds.w,
                patch.mask.bounds.h,
                params["elapsed_ms"],
                params.get("tiles").map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
                params["edge_pad"],
                params["quality"],
                if provenance.model_sha256.is_some() { "yes" } else { "none" },
                review.as_deref().map(|r| format!(" - {r}")).unwrap_or_default(),
            )
        }
        RegionOutcome::Untouched { bbox, reason } | RegionOutcome::Declined { bbox, reason, .. } => {
            format!("({}, {}) {}×{}: untouched - {reason}", bbox.x, bbox.y, bbox.w, bbox.h)
        }
        RegionOutcome::Candidate { bbox, reason, inside_bubble, .. } => {
            format!("({}, {}) {}×{}: candidate - {reason}, in a balloon: {inside_bubble}", bbox.x, bbox.y, bbox.w, bbox.h)
        }
        RegionOutcome::Detected(found) => {
            let bbox = found.record.bbox;
            format!("({}, {}) {}×{}: detected - {}", bbox.x, bbox.y, bbox.w, bbox.h, found.record.pick)
        }
    }
}

#[test]
fn the_pipeline_cleans_a_page_when_the_weights_and_the_runtime_are_present() {
    let _serial = one_gpu_at_a_time();
    let Some(mut pipeline) = the_real_pipeline() else { return };
    let Some(outcome) = clean_fixture(&mut pipeline, "page-dialogue.png", HIGHEST_LOCAL) else {
        return;
    };
    assert!(!outcome.regions.is_empty(), "a page of dialogue produced no regions at all");

    let cleaned: Vec<&Patch> = outcome
        .regions
        .iter()
        .filter_map(|region| match region {
            RegionOutcome::Cleaned(patch, _) => Some(&**patch),
            RegionOutcome::Untouched { .. } | RegionOutcome::Candidate { .. } | RegionOutcome::Declined { .. }
            | RegionOutcome::Detected(..) => None,
        })
        .collect();
    assert!(!cleaned.is_empty(), "no region of a page of dialogue was cleaned");
    for patch in &cleaned {
        assert!(patch.is_well_formed(), "{}: the pixels do not cover the mask", patch.id);
        assert!(patch.id.starts_with("c1-p001-r"));
        assert_eq!(patch.provenance.mask_sha256.len(), 64);
        assert_eq!(patch.provenance.engine, Engine::Fill);
        assert_eq!(patch.provenance.params_snapshot["fill_mode"], "match-surround");
        assert!(patch.provenance.params_snapshot["elapsed_ms"].is_u64());
        assert!(patch.provenance.cloud.is_none(), "a local run recorded a cloud request");
    }
}

/// Region ids are list indices, and the list has two sources.
///
/// The pipeline adopts the balloon detector's uncovered text boxes as regions
/// and then sorts the joined list by `(masking.y, masking.x)`, because
/// appending would give them the last ids on the page instead of their own
/// place in reading order. What a run emits is a
/// subsequence of that list - a region can be skipped for an empty seed or for
/// belonging to another segment - so the property survives into the outcome,
/// and this is where a lost `sort_regions` would show up.
#[test]
fn the_regions_a_page_reports_are_in_reading_order() {
    let _serial = one_gpu_at_a_time();
    let Some(mut pipeline) = the_real_pipeline() else { return };
    let Some(outcome) = clean_fixture(&mut pipeline, "page-crowded.png", HIGHEST_LOCAL) else {
        return;
    };
    let boxes: Vec<Rect> = outcome
        .regions
        .iter()
        .map(|region| match region {
            RegionOutcome::Cleaned(patch, _) => patch.mask.bounds,
            RegionOutcome::Untouched { bbox, .. } | RegionOutcome::Candidate { bbox, .. }
            | RegionOutcome::Declined { bbox, .. } => *bbox,
            RegionOutcome::Detected(found) => found.record.bbox,
        })
        .collect();
    assert!(boxes.len() > 1, "a crowded page reported {} regions", boxes.len());
    for pair in boxes.windows(2) {
        // The mask a rung wrote is grown inside the masking box, so the
        // comparison is on the row and not on the exact origin: what must hold
        // is that the page is walked downwards.
        assert!(
            pair[0].y <= pair[1].bottom(),
            "region at ({}, {}) came out after one at ({}, {})",
            pair[1].x,
            pair[1].y,
            pair[0].x,
            pair[0].y
        );
    }
}

/// The ladder, end to end, on the fixture it exists for.
///
/// `page-crowded.png` is the fixture that exercises it: twenty detected
/// regions, of which **seven** reach the ladder. `page-screentone.png` does
/// **not**, despite the name: the two regions the detector emits on it sit on
/// flat paper and fit at deviation 0. Run this with `--nocapture` and it prints
/// what every region actually did, which is the only form the evidence takes: a
/// wiring change that compiles is not one.
///
/// **None of the seven is on screentone**, and an earlier version of this
/// docstring said all eleven of them were. What actually puts them here is that
/// the generator's balloons are too small for their lettering: on each of the
/// seven a glyph overruns its own outline, so the fitted mask must cover a piece
/// of a 6 px ink stroke, §4's fifth correction refuses every candidate in the
/// series, and [`fit::fit_within`] falls back to the ladder. Four more regions
/// used to be here for a different reason - the mask cleared the stroke but the
/// four-pixel annulus did not - and [`cleaner_core::fit::ring::paper_annulus`]
/// is why they are at rung 0 now.
///
/// So this test rests on a **fixture defect**, and it should not rest on one.
/// The seven are exactly the regions the ladder handles worst: a hole inside a
/// speech balloon is where manga-LaMa invents Japanese text
/// ([`cleaner_core::engines::lama::model_hole`]), so the only end-to-end
/// evidence Phase 3 has that the ladder runs comes from seven regions the ladder
/// should arguably never have been given. A fixture with text genuinely on
/// screentone - which the detector currently finds on none of the four toned
/// panels - would replace it.
///
/// It is not a quality measurement. Phase 3's quality exit is a blind human A/B
/// on a screentone-heavy fixture, and `fixtures/pages/` is synthetic.
#[test]
fn a_crowded_page_reaches_rung_two_and_records_where_it_ran() {
    // A host under memory pressure surrenders rung 2 before this test's first
    // assertion, which is the ladder doing its job on the wrong machine.
    let level = memory::pressure();
    if level.is_elevated() {
        eprintln!("skipped: host is under memory pressure ({level:?})");
        return;
    }
    let _serial = one_gpu_at_a_time();
    let Some(mut pipeline) = the_real_pipeline() else { return };
    let started = Instant::now();
    let Some(outcome) = clean_fixture(&mut pipeline, "page-crowded.png", HIGHEST_LOCAL) else {
        return;
    };
    eprintln!("page-crowded.png in {:.2} s", started.elapsed().as_secs_f64());
    for region in &outcome.regions {
        eprintln!("  {}", one_line(region));
    }

    let inpainted: Vec<&Patch> = outcome
        .regions
        .iter()
        .filter_map(|region| match region {
            RegionOutcome::Cleaned(patch, _) => Some(&**patch),
            RegionOutcome::Untouched { .. } | RegionOutcome::Candidate { .. } | RegionOutcome::Declined { .. }
            | RegionOutcome::Detected(..) => None,
        })
        .filter(|patch| patch.provenance.engine == Engine::Lama)
        .collect();
    assert!(!inpainted.is_empty(), "no region of a crowded page reached rung 2");

    for patch in &inpainted {
        assert!(patch.is_well_formed(), "{}: the pixels do not cover the mask", patch.id);
        let provenance = &patch.provenance;
        // A model ran, so there **is** a digest and there is a provider that
        // chose itself - `None` here would be a missing answer rather than a
        // true one, which is what it is one rung down.
        assert_eq!(
            provenance.model_sha256.as_ref().map(|digest| digest.len()),
            Some(64),
            "{}: rung 2 recorded no model digest",
            patch.id
        );
        assert!(!provenance.execution_provider.is_empty());
        assert_eq!(provenance.params_snapshot["rung"], "ladder.rung.lama");
        assert_eq!(provenance.params_snapshot["fill_mode"], "reconstruct");
        assert_eq!(provenance.params_snapshot["quality"], "measured");
        assert!(
            provenance.params_snapshot["tiles"].as_u64().is_some_and(|tiles| tiles >= 1),
            "{}: a rung-2 patch that ran no tiles",
            patch.id
        );
        assert!(provenance.cloud.is_none(), "a local run recorded a cloud request");
    }

    // **Auto clean is local-only, and rung 2 is the top of the automatic
    // path.** At the highest local ceiling there is still nothing above it.
    for region in &outcome.regions {
        if let RegionOutcome::Cleaned(patch, _) = region {
            assert!(
                rung(patch.provenance.engine) <= rung(Engine::Lama),
                "{} ran at {:?}",
                patch.id,
                patch.provenance.engine
            );
        }
    }
}

/// **The shipped picks, on the page the ladder actually fires on.**
///
/// The test above is the same fixture with nobody asking: seven regions reach
/// rung 2 because the fit could not settle them, and every one of those seven
/// is text inside a speech balloon - which is where manga-LaMa invents Japanese
/// text, and is exactly the place a translator wants a fill and not a model.
/// With the tool window's own defaults those regions start on the fill
/// instead, and the page comes back with **no rung-2 patch at all**: not a
/// decline, not a hole - the fill's pixels pass the metric, which is the
/// whole argument for offering the row.
///
/// The unit tests either side of [`start_rung`] pin the arithmetic. This pins
/// that the arithmetic reaches the page, which is the claim the row makes to
/// the user.
#[test]
fn the_shipped_picks_keep_bubble_text_off_the_inpainter() {
    let _serial = one_gpu_at_a_time();
    let Some(pipeline) = the_real_pipeline() else { return };
    let mut pipeline = pipeline.with_picks(Picks::default());
    let Some(outcome) = clean_fixture(&mut pipeline, "page-crowded.png", HIGHEST_LOCAL) else {
        return;
    };

    let mut cleaned = 0usize;
    for region in &outcome.regions {
        if let RegionOutcome::Cleaned(patch, _) = region {
            cleaned += 1;
            assert!(
                rung(patch.provenance.engine) <= rung(Engine::Fill),
                "{} is bubble text and ran at {:?}",
                patch.id,
                patch.provenance.engine
            );
        }
    }
    // And the regions are still *cleaned* - a pick that turned seven patches
    // into seven declines would pass the assertion above and fail the user.
    assert!(cleaned > 0, "the picked run cleaned nothing at all");
}

/// **The pressure ladder, fired.**
///
/// A machine under memory pressure is not a state a test can arrange, so the
/// kernel's reading is supplied rather than waited for - everything downstream
/// of that one value is the real thing, on the real fixture, through the real
/// sessions.
///
/// What the step has to be is **recoverable**, and this is what that means in
/// outcomes: the regions that would have gone to rung 2 are left exactly as they
/// were and listed with `decline.reason.rungUnavailable`, every region
/// rung 0 can carry is still cleaned, and the page still comes back. A
/// run is a rung worse, not over.
///
/// The step used to have a second half - rung 3's session had to *not* be
/// opened instead, since reaching for the preview tier on a machine that had
/// just asked for memory back would be the step undoing itself. Rung 3 is
/// gone, so there is one session to give back and nothing left to reach for.
#[test]
fn a_run_under_pressure_gives_back_the_inpainter_and_keeps_going() {
    // The "before" half of this test asserts the host is *not* under pressure;
    // a host that already is fails that assertion before the test's own signal
    // is ever supplied.
    let level = memory::pressure();
    if level.is_elevated() {
        eprintln!("skipped: host is under memory pressure ({level:?})");
        return;
    }
    let _serial = one_gpu_at_a_time();
    let Some(mut pipeline) = the_real_pipeline() else { return };

    // First, with no pressure, so the comparison below is against this
    // machine's own answer rather than against an assumption.
    let Some(before) = clean_fixture(&mut pipeline, "page-crowded.png", HIGHEST_LOCAL) else {
        return;
    };
    let inpainted = before
        .regions
        .iter()
        .filter(|region| {
            matches!(region, RegionOutcome::Cleaned(patch, _)
                if rung(patch.provenance.engine) >= rung(Engine::Lama))
        })
        .count();
    assert!(inpainted > 0, "the fixture reached no model rung, so there is nothing to give back");
    assert!(before.pressure.is_empty(), "the ladder fired on a machine that is not under pressure");
    assert!(matches!(pipeline.rung2.state, Session::Held(_)), "rung 2 was never held");

    // The kernel says the machine is short.
    assert_eq!(
        pipeline.take_pressure(cleaner_core::memory::Pressure::Critical),
        Some("notice.memory.unloadedInpainter")
    );
    assert!(matches!(pipeline.rung2.state, Session::Surrendered));

    let Some(after) = clean_fixture(&mut pipeline, "page-crowded.png", HIGHEST_LOCAL) else {
        return;
    };
    let mut left_alone = 0;
    for region in &after.regions {
        match region {
            RegionOutcome::Cleaned(patch, _) => assert!(
                rung(patch.provenance.engine) < rung(Engine::Lama),
                "{} ran at {:?} after the inpainter was given back",
                patch.id,
                patch.provenance.engine
            ),
            RegionOutcome::Untouched { reason, .. } | RegionOutcome::Declined { reason, .. } => {
                if reason == "decline.reason.rungUnavailable" {
                    left_alone += 1;
                }
            }
            RegionOutcome::Candidate { .. } => {}
            RegionOutcome::Detected(..) => unreachable!("a one-pass clean stores nothing"),
        }
    }
    assert!(!after.regions.is_empty(), "a page under pressure produced nothing at all");
    assert_eq!(left_alone, inpainted, "the model regions were not the ones left alone");
    // The session is not rebuilt on the next region, or the next page: the step
    // is a latch and `Unopened` would be the state that reopens.
    assert!(matches!(pipeline.rung2.state, Session::Surrendered));

    // And the second step is the one rule 6 names, reported on the same stream.
    assert_eq!(
        pipeline.take_pressure(cleaner_core::memory::Pressure::Warn),
        Some("notice.memory.oneWindow")
    );
}

/// **Opening a pipeline opens no sessions.**
///
/// This needs no ONNX Runtime and no inference, which is the point: the whole
/// of `Pipeline::open` is now a filesystem check, so a run that is about to be
/// declined for some other reason never pays for a session. The three models it
/// used to build come to about 110 MB of weights and several seconds of session
/// build between them.
#[test]
fn opening_a_pipeline_costs_no_session() {
    let Some(models) = model_dir(None).or_else(|| {
        let up = PathBuf::from("../models");
        models_ready(&up).then_some(up)
    }) else {
        eprintln!("skipped: no model weights - run scripts/fetch-models.sh");
        return;
    };
    let pipeline = Pipeline::open(&models, Preference::Automatic).expect("the weights are there");
    assert!(!pipeline.detector.is_held(), "the detector was opened before a page asked");
    assert!(!pipeline.balloons.is_held());
    assert!(!pipeline.gate.is_held());
    assert!(matches!(pipeline.rung2.state, Session::Unopened));

    // And the failure that *is* worth reporting early still is.
    let empty = Scratch::new("no-models");
    assert!(Pipeline::open(&empty.join("."), Preference::Automatic).is_err());
}

/// **A partial model directory is not a model directory.**
///
/// `model_dir` used to decide whether weights were present by checking only
/// `DETECTOR`. If a user had downloaded only part of the suite (or if a
/// download was interrupted), `model_dir` picked that directory, `Pipeline::open`
/// failed on the next required file, and `start` propagated the error - turning
/// a missing-model condition into a rejected IPC promise instead of the warning
/// notice the frontend expects.
///
/// Every file required to open a pipeline (`DETECTOR`, `BALLOONS`, `GATE_MODEL`,
/// and `GATE_LABELS`) must be present before a candidate path is accepted.
#[test]
fn a_partial_model_directory_is_not_a_model_directory() {
    let scratch = Scratch::new("partial-models");
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();

    // With only the detector file, the directory is incomplete and must not be chosen.
    std::fs::write(models.join(DETECTOR), b"").unwrap();
    assert!(!models_ready(&models), "only detector should not be ready");
    assert_eq!(
        model_dir(Some(&scratch.0)),
        None,
        "partial directory was chosen when incomplete"
    );

    // With some but not all required files present, still incomplete.
    std::fs::write(models.join(BALLOONS), b"").unwrap();
    std::fs::write(models.join(GATE_MODEL), b"").unwrap();
    assert!(!models_ready(&models), "missing labels should not be ready");
    assert_eq!(
        model_dir(Some(&scratch.0)),
        None,
        "partial directory without labels was chosen"
    );

    // Once all required files are present, the directory is accepted.
    std::fs::write(models.join(GATE_LABELS), b"").unwrap();
    assert!(models_ready(&models), "all required models should be ready");
    assert_eq!(
        model_dir(Some(&scratch.0)),
        Some(models),
        "complete directory was not chosen"
    );
}

/// **A missing model file is a variant, and its wording is unchanged.**
///
/// `start` uses this as defence in depth: if `Pipeline::open` fails because a
/// required model file went missing between the `model_dir` check and the open -
/// a download deleted, a volume unmounted - it emits
/// `notice.run.modelsMissing` and answers an empty handle rather than rejecting
/// the IPC promise. It used to tell that failure apart by reading the front and
/// the back of the error *string*; now it matches `MissingModel`, and the
/// variant carries the path rather than leaving it to be parsed back out.
///
/// The message itself is asserted here because it is the one thing the type
/// change was not allowed to alter: it is what a `Result<_, String>` caller
/// shows, so `Display` must still produce exactly the sentence it produced when
/// it was a `format!`.
#[test]
fn a_partial_or_missing_model_error_is_a_typed_variant() {
    let empty = Scratch::new("missing-model-err");
    let models = empty.join(".");
    // `Pipeline` has no `Debug`, so `expect_err` is not available here.
    let error = match Pipeline::open(&models, Preference::Automatic) {
        Ok(_) => panic!("empty directory should fail to open"),
        Err(error) => error,
    };

    // The first required file, named - not a prefix a caller has to strip.
    let OpenError::MissingModel { path } = &error;
    assert_eq!(path, &models.join(DETECTOR), "the missing file was not named");

    // And the sentence the seam carries, character for character.
    let expected = format!("the model file {} is missing", models.join(DETECTOR).display());
    assert_eq!(error.to_string(), expected, "the message a caller shows changed");
    assert_eq!(String::from(error), expected, "the `String` conversion disagrees with `Display`");
}

/// **The close button in the loaded-models tab, end to end.**
///
/// A page is cleaned, which loads whatever that page needs; every row is then
/// asked for back the way `unload_model` asks; and the next page finds the
/// sessions gone - and reopens them, because a run cannot proceed without the
/// detector and this is not the pressure ladder's latch.
///
/// The distinction against
/// `a_run_under_pressure_gives_back_the_inpainter_and_keeps_going` is the whole
/// point: that one leaves rung 2 `Surrendered` and the page a rung worse, and
/// this one leaves everything `Unopened` and the page unchanged.
#[test]
fn a_requested_unload_is_taken_at_the_next_safe_point_and_the_run_goes_on() {
    let _serial = one_gpu_at_a_time();
    let Some(mut pipeline) = the_real_pipeline() else { return };
    let Some(before) = clean_fixture(&mut pipeline, "page-crowded.png", HIGHEST_LOCAL) else {
        return;
    };
    assert!(pipeline.detector.is_held(), "a cleaned page held no detector");
    let cleaned = |outcome: &PageOutcome| {
        outcome.regions.iter().filter(|r| matches!(r, RegionOutcome::Cleaned(..))).count()
    };
    assert!(cleaned(&before) > 0, "the fixture cleaned nothing, so there is nothing to compare");

    // What the tab does: one request per row, and not one of them touches a
    // session.
    let rows = cleaner_core::registry::loaded();
    assert!(rows.len() >= 3, "the detectors and the gate are not on the tab: {rows:?}");
    for row in &rows {
        assert!(cleaner_core::registry::request_unload(row.id));
    }
    // Still loaded: an unload is a request, and the run is between pages.
    assert!(pipeline.detector.is_held());

    let Some(after) = clean_fixture(&mut pipeline, "page-crowded.png", HIGHEST_LOCAL) else {
        return;
    };
    // Every row the tab was showing has gone. The ones that came back are new
    // sessions with new ids, which is what makes "gone" checkable at all.
    let ids: std::collections::HashSet<u64> = rows.iter().map(|row| row.id).collect();
    for row in cleaner_core::registry::loaded() {
        assert!(!ids.contains(&row.id), "a session that was asked for back is still loaded");
    }
    // And the page is unchanged: this is a reopen, not a rung lost.
    assert_eq!(cleaned(&after), cleaned(&before), "unloading cost the page its rungs");
}

/// **A model, once loaded, outlives the run that loaded it.**
///
/// The ruling this whole mechanism exists for, stated against the only thing
/// that can prove it: the registry's row ids. An id is minted per *session*, so
/// a second run finding the same ids is a second run that reopened nothing -
/// and a second run finding different ones would be exactly the reload cost
/// still being paid.
#[test]
fn a_session_outlives_the_run_that_opened_it_and_the_next_run_gets_it_back() {
    let _serial = one_gpu_at_a_time();
    let Some(mut pipeline) = the_real_pipeline() else { return };
    if clean_fixture(&mut pipeline, "page-crowded.png", HIGHEST_LOCAL).is_none() {
        return;
    }
    let opened: Vec<u64> = cleaner_core::registry::loaded().iter().map(|row| row.id).collect();
    assert!(opened.len() >= 3, "a cleaned page held fewer models than it needs: {opened:?}");

    // The run ends. **Nothing is unloaded**: the sessions go to the residency
    // cache, and their rows stay up because the sessions are still resident.
    drop(pipeline);
    let parked: Vec<u64> = cleaner_core::registry::loaded().iter().map(|row| row.id).collect();
    for id in &opened {
        assert!(parked.contains(id), "the end of a run unloaded session {id}");
    }

    // And the next run gets those very sessions rather than building new ones.
    let Some(mut second) = the_real_pipeline() else { return };
    if clean_fixture(&mut second, "page-crowded.png", HIGHEST_LOCAL).is_none() {
        return;
    }
    let now: Vec<u64> = cleaner_core::registry::loaded().iter().map(|row| row.id).collect();
    for id in &opened {
        assert!(now.contains(id), "the second run reopened session {id}");
    }
}

/// **One page, detected once**, however many region edits land on it.
///
/// The memo is keyed by the source's digest and the detection is a pure
/// function of the source's pixels, so the second call is the *same* detection -
/// asserted by pointer, which is the only assertion that cannot pass by
/// accident on a page that would have detected identically anyway.
#[test]
fn a_page_is_detected_once_however_many_edits_land_on_it() {
    let _serial = one_gpu_at_a_time();
    // For the runtime and the weights, which this borrows rather than restates.
    let Some(pipeline) = the_real_pipeline() else { return };
    drop(pipeline);
    let models = model_dir(None).or_else(|| {
        let up = PathBuf::from("../models");
        models_ready(&up).then_some(up)
    });
    let Some(models) = models else { return };
    let page = PathBuf::from("../fixtures/pages").join("page-crowded.png");
    let Ok(bytes) = std::fs::read(&page) else { return };
    let decoded = cleaner_core::image::decode(&bytes).expect("the fixture decoded");
    let digest = cleaner_core::ingest::sha256_hex(&bytes);

    cleaner_core::detect::forget();
    let mut detector = Detector::open(&models.join(DETECTOR), Preference::Automatic)
        .expect("the detector opened");
    let first = cleaner_core::detect::detect_page(&mut detector, &digest, &decoded)
        .expect("the page detected");
    let again = cleaner_core::detect::detect_page(&mut detector, &digest, &decoded)
        .expect("the page detected twice");
    assert!(std::sync::Arc::ptr_eq(&first, &again), "the second edit detected the page again");

    // A different source is a different page, memo or no memo.
    let other = cleaner_core::detect::detect_page(&mut detector, "not-this-page", &decoded)
        .expect("the other page detected");
    assert!(!std::sync::Arc::ptr_eq(&first, &other));
    cleaner_core::detect::forget();
}

/// The same page under a ceiling of rung 1: every region the ladder wanted is
/// **left alone**, and none of them is quietly filled by a rung §5 already
/// ruled out.
#[test]
fn a_capped_run_leaves_the_ladders_regions_alone_rather_than_filling_them() {
    let _serial = one_gpu_at_a_time();
    let Some(mut pipeline) = the_real_pipeline() else { return };
    let Some(outcome) = clean_fixture(&mut pipeline, "page-crowded.png", Engine::Fill) else {
        return;
    };
    for region in &outcome.regions {
        eprintln!("  {}", one_line(region));
    }
    let declined = outcome
        .regions
        .iter()
        .filter(|region| {
            matches!(region, RegionOutcome::Untouched { reason, .. } | RegionOutcome::Declined { reason, .. }
                if reason == "decline.reason.rungUnavailable")
        })
        .count();
    assert!(declined > 0, "a capped run cleaned every region of a crowded page");
    for region in &outcome.regions {
        if let RegionOutcome::Cleaned(patch, _) = region {
            assert!(
                rung(patch.provenance.engine) <= rung(Engine::Fill),
                "{} ran at {:?} under a rung-0 ceiling",
                patch.id,
                patch.provenance.engine
            );
        }
    }
}

/* ------------------------------------------------------------------ */
/* Rules 1 to 4, wired                                                */
/* ------------------------------------------------------------------ */

/// A longstrip chapter over `fixtures/pages/strip-01…03.png`.
///
/// Those three pages are one 800×12000 canvas cut into three, and the cut
/// between page 2 and page 3 deliberately repeats eight rows - a duplicated
/// overlap, the one join anomaly the catalogue has a sentence for.
fn a_longstrip_chapter(scratch: &Scratch) -> Option<(Library, String, String)> {
    let raws = scratch.join("strip-raws");
    std::fs::create_dir_all(&raws).unwrap();
    for n in 1..=3 {
        let from = PathBuf::from(format!("../fixtures/pages/strip-{n:02}.png"));
        let bytes = std::fs::read(&from).ok()?;
        std::fs::write(raws.join(format!("{n:03}.png")), bytes).unwrap();
    }
    let library = Library::at(scratch.join("library"));
    let project = library
        .create_project("Webtoon", StripMode::Longstrip, Some(raws), None)
        .unwrap();
    let chapter = library.create_chapter(&project.id, "Ch. 1", None, None).unwrap().created().unwrap();
    Some((library, project.id, chapter.id))
}

/// **Satisfies one half of what's needed.**
///
/// `check_join` and `Joins::anomalies` had no caller outside their own tests,
/// so "report them" was unmet for **four of four** variants, including the
/// one the catalogue can say. The survey is the caller, and this is the
/// notice arriving on the same stream the run's events ride.
///
/// What it does not close: the other three variants still have no key, so a
/// misregistration, a width mismatch and a 1-px seam are still found, still
/// treated as page edges, and still unreportable.
#[test]
fn a_duplicated_join_is_reported_on_the_stream() {
    let scratch = Scratch::new("join-anomaly");
    let Some((library, _, chapter_id)) = a_longstrip_chapter(&scratch) else {
        eprintln!("skipped: no fixtures/pages/strip-*.png - run scripts/make-fixture-pages.py");
        return;
    };
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    assert_eq!(entries.len(), 3);

    let recorder = Recorder::new();
    let mut stub = Stub::new(0, 0);
    execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut stub,
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    let anomalies: Vec<serde_json::Value> = recorder
        .events()
        .iter()
        .filter_map(|event| match event {
            Event::Notice { key, params, tone, .. } if key == "notice.input.joinAnomaly" => {
                assert_eq!(*tone, "warn");
                Some(params.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(anomalies.len(), 1, "the deliberately broken join was not reported");
    // The catalogue's sentence is "Duplicated overlap rows between positions
    // {first} and {second}", and the positions a reader counts from are 1.
    assert_eq!(anomalies[0]["first"], 2);
    assert_eq!(anomalies[0]["second"], 3);

    // And the notice is a notice: it does not disturb the five ordered events.
    let ordered: Vec<&'static str> =
        recorder.kinds().into_iter().filter(|kind| *kind != "notice").collect();
    assert_eq!(
        ordered,
        vec![
            "page-started", "page-done", "page-started", "page-done", "page-started", "page-done",
            "run-finished",
        ]
    );
}

/// The shipped strip is a single webtoon canvas cut into source pages. With
/// real detector, balloon and script-gate sessions, the clean pass must keep a
/// region at its verified first join whole and must not emit its overlap again
/// when the second page is processed.
#[test]
fn the_real_pipeline_persists_one_patch_across_a_verified_join() {
    let _serial = one_gpu_at_a_time();
    let Some(mut pipeline) = the_real_pipeline() else { return };
    let scratch = Scratch::new("real-cross-join");
    let Some((library, _, chapter_id)) = a_longstrip_chapter(&scratch) else { return };
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let recorder = Recorder::new();
    execute(
        "cross-join",
        &chapter_id,
        &entries,
        &mut pipeline,
        Engine::Fill,
        &recorder.cancel,
        &recorder.sink(),
    );

    let job = Job::open(&entries[0].job_path).unwrap();
    let first_height = job.project.sources[entries[0].source_idx].h as i64;
    let spanning: Vec<_> = job.project.patches.iter().filter_map(|record| {
        (record.source_idx == entries[0].source_idx)
            .then(|| job.load_patch(record).ok())
            .flatten()
            .filter(|patch| patch.mask.bounds.y < first_height
                && patch.mask.bounds.bottom() > first_height)
    }).collect();
    assert_eq!(spanning.len(), 1, "the verified join must produce one anchored spanning patch");

    let strip = strip_of(&job.project);
    let lifted = cleaner_core::export::StripPatch::lift(&strip, 0, &spanning[0]).unwrap();
    assert!(cleaner_core::export::patches_on_page(&strip, 0, std::slice::from_ref(&lifted)).len() == 1);
    let on_second = cleaner_core::export::patches_on_page(&strip, 1, std::slice::from_ref(&lifted));
    assert_eq!(on_second.len(), 1);
    let expected = on_second[0].mask.bounds;
    let duplicate = job.project.patches.iter().filter(|record| record.source_idx == entries[1].source_idx)
        .filter_map(|record| job.load_patch(record).ok())
        .any(|patch| {
            let other = patch.mask.bounds;
            let w = (expected.right().min(other.right()) - expected.x.max(other.x)).max(0);
            let h = (expected.bottom().min(other.bottom()) - expected.y.max(other.y)).max(0);
            let intersection = w * h;
            let smaller = (i64::from(expected.w) * i64::from(expected.h))
                .min(i64::from(other.w) * i64::from(other.h));
            smaller > 0 && intersection * 2 >= smaller
        });
    assert!(!duplicate, "the second page emitted a clipped duplicate of the spanning patch");
}

/// **Rule 2's plan, recorded.** `strip.splits` had no writer at all - the
/// field's own doc comment says so - and rule 2 requires a fallback to be
/// "recorded in `strip.splits` and surfaced in review".
///
/// The fixture is three 4000 px pages, so every join is inside a tolerance band
/// at some point and rule 2's "page joins win unconditionally" is what the plan
/// is mostly made of.
#[test]
fn the_split_plan_reaches_the_manifest() {
    let scratch = Scratch::new("splits");
    let Some((library, project_id, chapter_id)) = a_longstrip_chapter(&scratch) else {
        eprintln!("skipped: no fixtures/pages/strip-*.png - run scripts/make-fixture-pages.py");
        return;
    };
    let job_path = library.job_path(&project_id, &chapter_id);
    assert!(Job::open(&job_path).unwrap().project.strip.splits.is_empty(), "before the run");

    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let recorder = Recorder::new();
    let mut stub = Stub::new(0, 0);
    execute(
        "run-1",
        &chapter_id,
        &entries,
        &mut stub,
        Engine::Flux,
        &recorder.cancel,
        &recorder.sink(),
    );

    let splits = Job::open(&job_path).unwrap().project.strip.splits;
    assert!(!splits.is_empty(), "rule 2's plan was not recorded");
    let rows: Vec<u32> = splits.iter().map(|s| s.y).collect();
    assert!(rows.windows(2).all(|w| w[0] < w[1]), "the plan runs backwards: {rows:?}");
    assert!(rows.iter().all(|y| *y > 0 && *y < 12_000), "{rows:?}");
    // The two page joins are free, guaranteed text-free split points and rule 2
    // takes them unconditionally.
    for join in [4_000u32, 8_000] {
        let taken = splits.iter().find(|s| s.y == join).expect("a join was not taken");
        assert_eq!(taken.kind, SplitKind::Join, "{taken:?}");
    }
}

/// The cost of the survey, stated as the rule that decides whether to pay it.
///
/// A chapter of ordinary pages pays nothing: its pages are its segments, which
/// is rule 2's own sentence for the case its trigger does not fire, and it
/// needs no join check because the join check scopes that to longstrip.
#[test]
fn an_ordinary_chapter_is_not_surveyed_and_a_longstrip_one_is() {
    let scratch = Scratch::new("survey-cost");
    let (library, project_id, chapter_id) = a_chapter(&scratch, 3);
    let job = Job::open(&library.job_path(&project_id, &chapter_id)).unwrap();
    assert!(!survey_is_worth_it(&job.project));
    // …and it still gets a strip, a segment per page and unchecked joins.
    let strip = strip_of(&job.project);
    assert_eq!(strip.pages().len(), 3);
    assert_eq!(strip.joins(), 2);

    let strip_scratch = Scratch::new("survey-cost-strip");
    let Some((library, project_id, chapter_id)) = a_longstrip_chapter(&strip_scratch) else {
        eprintln!("skipped: no fixtures/pages/strip-*.png - run scripts/make-fixture-pages.py");
        return;
    };
    let job = Job::open(&library.job_path(&project_id, &chapter_id)).unwrap();
    assert!(survey_is_worth_it(&job.project));
    assert_eq!(strip_of(&job.project).height(), 12_000);
}

/// **Rule 1's clamp, on the path a region actually takes.** The window is
/// computed in strip coordinates and ends in `Strip::clamp`, so a box on the
/// narrow page of a strip cannot reach the gutter beside it and a box beside an
/// unverified join cannot reach across it.
///
/// Asserted here rather than only in `cleaner_core::strip` because what this is
/// about is the *caller*: the numbers below are the ones `PageContext` hands to
/// `decode_window`.
#[test]
fn a_regions_decode_window_is_clamped_by_the_strip_it_sits_in() {
    let strip = Strip::of_sizes(&[(800, 4_000), (700, 4_000), (800, 4_000)]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip,
        joins: &survey.joins,
        segments: &survey.segments,
        placement: 1,
        sources: &[],
    };

    // A box hard against the narrow page's left edge, in that page's own
    // coordinates, in a segment that starts at the top of the page.
    let on_page = Rect::new(0, 100, 120, 60);
    let in_strip = context.to_strip(on_page, 0);
    assert_eq!(in_strip, Rect::new(50, 4_100, 120, 60), "the page is centred, so x is inset by 50");

    let window = cleaner_core::strip::decode_window(
        context.strip,
        context.joins,
        in_strip,
        2.0,
        cleaner_core::strip::EngineContext::None,
    );
    assert_eq!(window.rect.x, 50, "the window reached into the gutter");
    assert!(window.requested.x < 50);
    assert_eq!(window.pad, EdgePad::Replicate);
    // …and back in the page's own coordinates it starts at zero, which is what
    // the fit is clamped to.
    let decoded = fixtures::by_name("l8").raster;
    let bounds = context.to_crop(window.rect, 0, &decoded);
    assert_eq!(bounds.x, 0);
}

#[test]
fn detection_on_a_second_page_never_anchors_to_an_unverified_previous_join() {
    let strip = Strip::of_sizes(&[(800, 4_000), (800, 4_000)]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext { strip: &strip, joins: &survey.joins,
        segments: &survey.segments, placement: 1, sources: &[] };
    let segment = context.page_segments()[0];
    let window = detection_window(&context, segment, None);
    assert_eq!(window.rect.y, 4_000);
    assert_eq!(window.rect.bottom(), segment.detect_end as i64);
    assert!(window.rect.bottom() > 4_000, "the current page, not its predecessor, is read");
}

#[test]
fn engine_context_at_a_second_page_top_stays_anchored_on_that_page() {
    let strip = Strip::of_sizes(&[(800, 4_000), (800, 4_000)]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext { strip: &strip, joins: &survey.joins,
        segments: &survey.segments, placement: 1, sources: &[] };
    let region = Rect::new(200, 4_005, 80, 40);
    let window = anchored_engine_window(&context, region, 1.0, EngineContext::Local);
    assert_eq!(window.rect.y, 4_000);
    assert!(window.rect.bottom() > region.bottom());
    assert_eq!(window.pad, EdgePad::Replicate);
}

/* ------------------------------------------------------------------ */
/* Cloud detection stages                                              */
/* ------------------------------------------------------------------ */

mod cloud_stages {
    use super::*;
    use crate::inference::analysis::AnalysisGateway;
    use crate::inference::run_analysis::{AnalysisTargets, RemoteDetection, RemotePage, RunGrantScope};
    use cleaner_core::balloon::BalloonBox;
    use cleaner_core::cloud_analysis_wire::{AnalysisCapabilities, AnalysisRequest, AnalysisResult, RT, SAM};
    use cleaner_core::engines::render::CloudProvider;

    /// A gateway the tests below never reach: the page's answer is set by hand.
    struct Unreachable;
    impl AnalysisGateway for Unreachable {
        fn capabilities(&self) -> Result<AnalysisCapabilities, String> { Err("not called".into()) }
        fn analyze(&self, _: &AnalysisRequest, _: &[u8]) -> Result<AnalysisResult, String> { Err("not called".into()) }
    }

    fn remote(capabilities: &[&str]) -> RemoteDetection {
        RemoteDetection::new(Box::new(Unreachable), RunGrantScope {
            chapter_id: "chapter".into(), page_indices: vec![0],
            capabilities: capabilities.iter().map(|id| id.to_string()).collect(),
            provider: CloudProvider::Modal, profile_id: "modal-prof-1".into(),
            endpoint_fingerprint: "f".repeat(64), profile_epoch: 0, models: vec![],
        })
    }

    #[test]
    fn a_cloud_run_needs_consent_one_chapter_and_a_cloud_stage() {
        let none: Vec<String> = vec![];
        let sam = vec![SAM.to_string()];
        assert_eq!(cloud_refusal(&none, "page", false), None);
        assert_eq!(cloud_refusal(&none, "page", true), Some("cloud_run_grant_mismatch: capabilities"));
        assert_eq!(cloud_refusal(&sam, "page", false), Some("cloud_run_grant_required"));
        assert_eq!(cloud_refusal(&sam, "project", true), Some("cloud_run_scope_unsupported"));
        assert_eq!(cloud_refusal(&sam, "chapter", true), None);
        // A cloud target on a stage the run does not select sends nothing.
        let targets = AnalysisTargets { rt_full: true, sam: true };
        let small = DetectorModels::from_args(Some(&serde_json::json!(["ctd", "rtSmall"]))).unwrap();
        assert!(targets.capabilities(small.rt_full, small.sam).is_empty());
        let full = DetectorModels::from_args(Some(&serde_json::json!(["rtFull", "samTs"]))).unwrap();
        assert_eq!(targets.capabilities(full.rt_full, full.sam), vec![SAM.to_string(), RT.to_string()]);
    }

    /// A cloud run takes CTD beside its cloud stages (the interface sends CTD,
    /// Full and SAM-TS-L), and refuses only the Small profile, which has no
    /// cloud twin. Nothing is refused while every stage stays here, nor in a
    /// clean, which detects nothing.
    #[test]
    fn cloud_detect_takes_ctd_and_refuses_only_small() {
        let cloud = AnalysisTargets { rt_full: true, sam: true };
        let local = AnalysisTargets::default();
        let models = |ids: serde_json::Value| DetectorModels::from_args(Some(&ids)).unwrap();
        let best = models(serde_json::json!(["ctd", "rtFull", "samTs"]));
        let ctd_sam = models(serde_json::json!(["ctd", "samTs"]));
        let ctd_only = models(serde_json::json!(["ctd"]));
        let small = models(serde_json::json!(["rtSmall"]));
        let small_sam = models(serde_json::json!(["rtSmall", "samTs"]));
        let ctd_small_sam = models(serde_json::json!(["ctd", "rtSmall", "samTs"]));
        let remote = models(serde_json::json!(["rtFull", "samTs"]));
        for mode in [RunMode::Detect, RunMode::Auto] {
            assert_eq!(cloud_detect_refusal(mode, cloud, best), None);
            assert_eq!(cloud_detect_refusal(mode, cloud, ctd_sam), None);
            assert_eq!(cloud_detect_refusal(mode, cloud, remote), None);
            // Nothing goes to the cloud with neither cloud stage selected.
            assert_eq!(cloud_detect_refusal(mode, cloud, ctd_only), None);
            assert_eq!(cloud_detect_refusal(mode, cloud, small), None);
            assert_eq!(cloud_detect_refusal(mode, cloud, small_sam), Some("cloud_detect_small_unsupported"));
            assert_eq!(cloud_detect_refusal(mode, cloud, ctd_small_sam), Some("cloud_detect_small_unsupported"));
            assert_eq!(cloud_detect_refusal(mode, local, small_sam), None);
        }
        assert_eq!(cloud_detect_refusal(RunMode::Clean, cloud, small_sam), None);
        // The cloud combination still sends exactly the two cloud stages.
        assert_eq!(cloud.capabilities(best.rt_full, best.sam), vec![SAM.to_string(), RT.to_string()]);
    }

    #[test]
    fn page_answers_are_cut_to_each_detection_crop() {
        let mut mask = vec![0u8; 8 * 6];
        mask[2 * 8 + 5] = 255;
        let page = RemotePage { width: 8, height: 6, mask: Some(mask), boxes: vec![
            BalloonBox { rect: Rect::new(4, 1, 4, 3), class: BalloonClass::TextFree, score: 0.8 },
            BalloonBox { rect: Rect::new(0, 5, 2, 1), class: BalloonClass::Bubble, score: 0.5 },
        ] };
        let (boxes, cut) = remote_for_crop(&page, &[], 4, 4, (3, 0));
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].rect, Rect::new(1, 1, 3, 3));
        let cut = cut.unwrap();
        assert_eq!(cut.len(), 16);
        assert_eq!(cut[2 * 4 + 2], 255);
        assert_eq!(cut.iter().filter(|value| **value == 255).count(), 1);
        // A crop reaching past the page reads background there.
        let (_, outside) = remote_for_crop(&page, &[], 4, 4, (6, 4));
        assert!(outside.unwrap().iter().all(|value| *value == 0));
        assert_eq!(cloud_failure_code("cloud_run_profile_changed"), "cloud_run_profile_changed");
        assert_eq!(cloud_failure_code("capability_unavailable: model identity changed"), "capability_unavailable");
        assert_eq!(cloud_failure_code("transport error: reset"), "cloud_analysis_failed");
    }

    /// A long strip's crop runs past the page into the next one: that page's
    /// answer fills the rows under the join, and a bubble the join cut into a
    /// top half and a bottom half is one box again.
    #[test]
    fn a_crop_across_a_join_reads_the_next_pages_answer() {
        let mut upper_mask = vec![0u8; 8 * 6];
        upper_mask[5 * 8 + 3] = 255;
        let upper = RemotePage { width: 8, height: 6, mask: Some(upper_mask), boxes: vec![
            BalloonBox { rect: Rect::new(2, 3, 4, 3), class: BalloonClass::Bubble, score: 0.6 },
            BalloonBox { rect: Rect::new(0, 0, 2, 2), class: BalloonClass::TextFree, score: 0.5 },
        ] };
        let mut lower_mask = vec![0u8; 8 * 6];
        lower_mask[3] = 200;
        let lower = RemotePage { width: 8, height: 6, mask: Some(lower_mask), boxes: vec![
            // The cut bubble's bottom half, and a box of another class on the join.
            BalloonBox { rect: Rect::new(2, 0, 4, 2), class: BalloonClass::Bubble, score: 0.9 },
            BalloonBox { rect: Rect::new(6, 0, 2, 2), class: BalloonClass::TextFree, score: 0.4 },
        ] };
        let below = [((0, 6), lower)];
        let (boxes, mask) = remote_for_crop(&upper, &below, 8, 12, (0, 0));
        let mut rects: Vec<Rect> = boxes.iter().map(|item| item.rect).collect();
        rects.sort_by_key(|rect| (rect.x, rect.y));
        assert_eq!(rects, vec![Rect::new(0, 0, 2, 2), Rect::new(2, 3, 4, 5), Rect::new(6, 6, 2, 2)]);
        let joined = boxes.iter().find(|item| item.rect == Rect::new(2, 3, 4, 5)).unwrap();
        assert_eq!((joined.class, joined.score), (BalloonClass::Bubble, 0.9));
        let mask = mask.unwrap();
        assert_eq!((mask[5 * 8 + 3], mask[6 * 8 + 3]), (255, 200));
        assert_eq!(mask.iter().filter(|value| **value != 0).count(), 2);
        // With no answer below, the rows past the page are background.
        let (_, alone) = remote_for_crop(&upper, &[], 8, 12, (0, 0));
        assert!(alone.unwrap()[6 * 8..].iter().all(|value| *value == 0));
    }

    /// With both stages in the cloud nothing local is opened: the boxes are
    /// the cloud's RT-DETR boxes and the segmentation is the cloud's mask.
    #[test]
    fn cloud_stages_replace_the_local_models_in_the_same_slots() {
        let dir = Scratch::new("cloud-stages");
        let models = DetectorModels::from_args(Some(&serde_json::json!(["rtFull", "samTs"]))).unwrap();
        let mut pipeline = Pipeline::open_selected(&dir.0, Preference::CpuOnly, models, true, None)
            .unwrap().with_remote(Some(remote(&[SAM, RT])));
        let mut mask = vec![0u8; 32 * 32];
        for y in 10..14 { for x in 10..20 { mask[y * 32 + x] = 255; } }
        pipeline.remote_page = Some(RemotePage { width: 32, height: 32, mask: Some(mask.clone()), boxes: vec![
            BalloonBox { rect: Rect::new(8, 8, 14, 8), class: BalloonClass::TextFree, score: 0.9 },
        ] });
        let mut crop = fixtures::by_name("l8").raster;
        crop.width = 32;
        crop.height = 32;
        crop.data = vec![200; 32 * 32];
        let SegmentDetection { balloons, detection, grouping, evidence } =
            pipeline.detect_segment(&crop, 0, (0, 0), "").unwrap();
        assert_eq!(balloons.len(), 1);
        assert_eq!(balloons[0].rect, Rect::new(8, 8, 14, 8));
        assert_eq!(detection.segmentation.levels, mask);
        // The cloud mask and box become one text group, the cleaning unit,
        // with its evidence encoded for the manifest.
        let groups: Vec<_> = grouping.cleaning().collect();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].box_ids, vec!["rt-0000".to_owned()]);
        assert!(groups[0].models.contains(&cleaner_core::text_groups::EvidenceModel::SamTsL));
        assert!(evidence.reference().starts_with("evidence/"));
        assert!(pipeline.full_rt.is_none() && pipeline.sam.is_none(), "no local session was opened");

        // A page with no cloud answer is refused, never analyzed locally instead.
        pipeline.remote_page = None;
        assert!(pipeline.detect_segment(&crop, 0, (0, 0), "").is_err());
        assert!(pipeline.full_rt.is_none() && pipeline.sam.is_none());
    }

    /// A real pipeline over a real chapter, its detection in the cloud through
    /// a fake gateway that fails some pages. `fail` maps a page index to the
    /// transport's words for its failure.
    fn cloud_run(name: &str, profile: &str, fail: &[(usize, String)])
        -> (Scratch, crate::inference::run_analysis::fake::Gateway, Vec<String>, Summary, Vec<Event>) {
        use cleaner_core::detect::{Detection, Letterbox, Region, Segmentation};
        use crate::inference::run_analysis::fake;
        fn detect(crop: &Raster, _: usize) -> Detection {
            Detection { boxes: Vec::new(), segmentation: Segmentation { width: crop.width, height: crop.height,
                levels: vec![0; (crop.width * crop.height) as usize], fit: Letterbox::fit(crop.width, crop.height) } }
        }
        fn balloons(_: &Raster, _: usize) -> Vec<BalloonBox> { Vec::new() }
        fn judge(_: &Region) -> Verdict { unreachable!("no region is detected") }

        let scratch = Scratch::new(name);
        let (library, _, chapter_id) = a_chapter(&scratch, 4);
        let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
        let shas: Vec<String> = entries.iter().map(|entry| {
            let job = Job::open(&entry.job_path).unwrap();
            sha256_hex(&std::fs::read(job.source_path(entry.source_idx).unwrap()).unwrap())
        }).collect();
        let gateway = fake::gateway();
        for (page, error) in fail {
            gateway.fail.lock().unwrap().insert(shas[*page].clone(), error.clone());
        }
        let pages: Vec<u32> = entries.iter().map(|entry| entry.page_index as u32).collect();
        let recorder = Recorder::new();
        let mut remote = RemoteDetection::new(Box::new(gateway.clone()), fake::scope(profile, &pages, &[SAM]));
        remote.set_cancel(Arc::clone(&recorder.cancel));

        let models = scratch.join("models");
        std::fs::create_dir_all(&models).unwrap();
        for name in REQUIRED_MODELS {
            std::fs::write(models.join(name), b"").unwrap();
        }
        let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap().with_remote(Some(remote));
        pipeline.test_vision = Some(TestVision { detect, balloons, judge });
        // The run's cloud notices go out through the global registry; they are
        // merged into the run's own stream here so their order can be read.
        let seen = Arc::clone(&recorder.seen);
        let subscription = events::register(Box::new(move |event| {
            if let Event::Notice { key, .. } = event {
                if key.starts_with("cloud.analysis.run.") {
                    seen.lock().unwrap().push(event.clone());
                }
            }
        }));
        pipeline.prefetch_remote(&entries);
        assert!(pipeline.remote.is_none() && pipeline.remote_sam(), "the producer holds the cloud stages");
        let summary = execute("run-1", &chapter_id, &entries, &mut pipeline, Engine::Flux, &recorder.cancel,
            &recorder.sink());
        events::unregister(subscription);
        drop(pipeline);
        (scratch, gateway, shas, summary, recorder.events())
    }

    /// Detect with its detection in the cloud stores the regions as the
    /// cloud's, sends every page once and releases the GPU once; a Clean after
    /// it sends nothing, and an Auto over the same pages would send none.
    #[test]
    fn a_cloud_detect_run_stores_cloud_detections_and_the_clean_sends_nothing() {
        use crate::inference::run_analysis::fake;
        let _exclusive = exclusively();
        let scratch = Scratch::new("cloud-detect");
        let (library, chapter_id) = a_lettered_chapter(&scratch, 2);
        let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Detect).unwrap();
        let shas: Vec<String> = entries.iter().map(|entry| {
            let job = Job::open(&entry.job_path).unwrap();
            sha256_hex(&std::fs::read(job.source_path(entry.source_idx).unwrap()).unwrap())
        }).collect();
        let gateway = fake::gateway();
        let pages: Vec<u32> = entries.iter().map(|entry| entry.page_index as u32).collect();
        let recorder = Recorder::new();
        let mut remote = RemoteDetection::new(Box::new(gateway.clone()),
            fake::scope("modal-prof-run-detect", &pages, &[SAM]));
        remote.set_cancel(Arc::clone(&recorder.cancel));
        let mut pipeline = lettered_pipeline(&scratch).with_remote(Some(remote));
        pipeline.detection_models.sam = true;
        pipeline.prefetch_remote(&pages_to_detect(&entries, RunMode::Detect));
        let summary = execute_as(RunMode::Detect, "run-1", &chapter_id, &entries, &mut pipeline, Engine::Lama,
            &recorder.cancel, &recorder.sink());
        drop(pipeline);
        assert_eq!((summary.regions_detected, summary.errored), (4, 0));
        assert_eq!(*gateway.seen.lock().unwrap(), shas, "each page once, in the run's order");
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);
        let job = Job::open(&entries[0].job_path).unwrap();
        assert!(job.project.detections.iter().all(|row| row.detector == "cloud"));
        assert!(job.project.examined.is_empty());

        let auto = plan_for(&library, "chapter", &chapter_id, None, RunMode::Auto).unwrap();
        assert_eq!(auto.len(), 2);
        assert!(pages_to_detect(&auto, RunMode::Auto).is_empty(), "no page goes to the cloud again");

        let sent = gateway.seen.lock().unwrap().len();
        let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Clean).unwrap();
        let recorder = Recorder::new();
        let mut cleaner = cleaning_only_pipeline(&scratch);
        let summary = execute_as(RunMode::Clean, "run-2", &chapter_id, &entries, &mut cleaner, Engine::Lama,
            &recorder.cancel, &recorder.sink());
        assert_eq!((summary.regions_cleaned, summary.errored), (2, 0));
        assert_eq!(gateway.seen.lock().unwrap().len(), sent, "a clean sent nothing");
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);
    }

    /// A cloud Detect or Auto over a long strip: each page is sent once, in
    /// order, and a window that crosses a join reads the next page's answer
    /// rather than sending that page again or reading background there.
    #[test]
    fn a_cloud_run_over_a_long_strip_reads_across_its_joins_and_sends_each_page_once() {
        use crate::inference::run_analysis::fake;
        let _exclusive = exclusively();
        for (mode, prefetch) in [(RunMode::Detect, true), (RunMode::Auto, false)] {
            let scratch = Scratch::new(if prefetch { "cloud-strip-prefetch" } else { "cloud-strip-inline" });
            let Some((library, _, chapter_id)) = a_longstrip_chapter(&scratch) else {
                eprintln!("skipped: no fixtures/pages/strip-*.png - run scripts/make-fixture-pages.py");
                return;
            };
            let entries = plan_for(&library, "chapter", &chapter_id, None, mode).unwrap();
            let shas: Vec<String> = entries.iter().map(|entry| {
                let job = Job::open(&entry.job_path).unwrap();
                sha256_hex(&std::fs::read(job.source_path(entry.source_idx).unwrap()).unwrap())
            }).collect();
            let gateway = fake::gateway();
            let pages: Vec<u32> = entries.iter().map(|entry| entry.page_index as u32).collect();
            let recorder = Recorder::new();
            let mut remote = RemoteDetection::new(Box::new(gateway.clone()),
                fake::scope("modal-prof-strip", &pages, &[SAM]));
            remote.set_cancel(Arc::clone(&recorder.cancel));
            let mut pipeline = lettered_pipeline(&scratch).with_remote(Some(remote));
            pipeline.detection_models.sam = true;
            if prefetch { pipeline.prefetch_remote(&pages_to_detect(&entries, mode)); }
            let summary = execute_as(mode, "run-strip", &chapter_id, &entries, &mut pipeline, Engine::Lama,
                &recorder.cancel, &recorder.sink());
            assert_eq!(summary.errored, 0, "{mode:?}");
            assert!(pipeline.answers_below > 0, "{mode:?}: no window read across a join");
            drop(pipeline);
            // One upload per tile: a tall page is several, and each page's
            // tiles go once, together, in the run's order.
            let mut seen = gateway.seen.lock().unwrap().clone();
            let tiles = seen.iter().filter(|sha| **sha == shas[0]).count();
            assert_eq!(seen.len(), tiles * shas.len(), "{mode:?}: a page was sent twice");
            seen.dedup();
            assert_eq!(seen, shas, "{mode:?}: each page once, in order");
        }
    }

    #[test]
    fn an_auto_page_that_gained_detections_after_planning_is_not_uploaded() {
        use crate::inference::run_analysis::fake;
        let _exclusive = exclusively();
        let scratch = Scratch::new("auto-cloud-plan-race");
        let (library, chapter_id) = a_lettered_chapter(&scratch, 1);
        let auto = plan_for(&library, "chapter", &chapter_id, None, RunMode::Auto).unwrap();
        assert_eq!(pages_to_detect(&auto, RunMode::Auto).len(), 1);

        let recorder = Recorder::new();
        let mut detector = lettered_pipeline(&scratch);
        execute_as(RunMode::Detect, "run-detect", &chapter_id, &auto, &mut detector,
            Engine::Lama, &recorder.cancel, &recorder.sink());
        assert!(!only_job(&library, &chapter_id).project.detections.is_empty());

        let gateway = fake::gateway();
        let remote = RemoteDetection::new(Box::new(gateway.clone()),
            fake::scope("modal-prof-auto-race", &[0], &[SAM]));
        let mut pipeline = cleaning_only_pipeline(&scratch).with_remote(Some(remote));
        pipeline.detection_models.sam = true;
        let recorder = Recorder::new();
        execute_as(RunMode::Auto, "run-auto", &chapter_id, &auto, &mut pipeline,
            Engine::Lama, &recorder.cancel, &recorder.sink());
        assert!(gateway.seen.lock().unwrap().is_empty(), "stored detections never upload the page");
    }

    /// Where each cloud notice sits in the run's stream: the page whose
    /// `page-started` it follows.
    fn notices_by_page(events: &[Event]) -> Vec<(u32, String, serde_json::Value)> {
        let mut page = None;
        events.iter().filter_map(|event| match event {
            Event::PageStarted { page_index, .. } => { page = Some(*page_index); None }
            Event::Notice { key, params, .. } if key.starts_with("cloud.analysis.run.") =>
                Some((page.expect("a notice before any page"), key.clone(), params.clone())),
            _ => None,
        }).collect()
    }

    /// Detection runs ahead, and a page it failed on is reported at that page,
    /// after the pages before it were cleaned, and the run goes on past it.
    /// Every page is sent once, and the GPU is released once, at the end.
    #[test]
    fn a_prefetched_failure_is_reported_at_its_page_and_the_run_goes_on() {
        let _exclusive = exclusively();
        let (_scratch, gateway, shas, summary, events) = cloud_run("prefetch-failed", "modal-prof-run-prefetch",
            &[(2, "network connection error".into())]);
        assert_eq!(notices_by_page(&events), vec![(2, "cloud.analysis.run.pageFailed".into(),
            serde_json::json!({ "page": 3, "code": "cloud_analysis_failed" }))]);
        assert_eq!((summary.pages_cleaned, summary.errored, summary.reason), (3, 1, "completed"));
        assert_eq!(*gateway.seen.lock().unwrap(), shas, "each page once, in the run's order");
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);
    }

    /// A failure that withdraws the run's authority ends the run at its page
    /// and the producer with it: nothing after that page is sent.
    #[test]
    fn a_prefetched_failure_that_stops_the_run_stops_the_producer() {
        let _exclusive = exclusively();
        let refused = crate::inference::http::HttpTransportError::UnexpectedStatus { status: 401 }.to_string();
        let (_scratch, gateway, shas, summary, events) = cloud_run("prefetch-stops", "modal-prof-run-prefetch-stop",
            &[(1, refused)]);
        assert_eq!(notices_by_page(&events), vec![
            (1, "cloud.analysis.run.pageFailed".into(), serde_json::json!({ "page": 2, "code": "gateway_unauthorized" })),
            (1, "cloud.analysis.run.stopped".into(), serde_json::json!({ "page": 2, "code": "gateway_unauthorized" })),
        ]);
        assert_eq!((summary.pages_cleaned, summary.errored), (1, 1));
        assert_eq!(*gateway.seen.lock().unwrap(), shas[..2].to_vec());
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);
    }
}

/// A page that fails in the cloud fails alone and the run goes on, as a local
/// failure does. A failure that withdraws the run's cloud authority ends the
/// walk after that page, says so, and leaves the rest unexamined for a later
/// run rather than knocking on the endpoint once per page.
#[test]
fn a_withdrawn_cloud_authority_ends_the_walk_and_other_failures_do_not() {
    let scratch = Scratch::new("cloud-stop");
    let (library, _, chapter_id) = a_chapter(&scratch, 4);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let mut stub = Stub::new(1, 0);
    stub.fails = vec![library::page_id(&chapter_id, 0)];
    stub.stops = vec![library::page_id(&chapter_id, 2)];
    let pages = Arc::clone(&stub.pages);
    let recorder = Recorder::new();
    let summary = execute("run-1", &chapter_id, &entries, &mut stub, Engine::Flux, &recorder.cancel, &recorder.sink());
    assert_eq!(*pages.lock().unwrap(), vec![
        library::page_id(&chapter_id, 0), library::page_id(&chapter_id, 1), library::page_id(&chapter_id, 2),
    ], "page 3 must not be tried");
    assert_eq!(summary.errored, 2);
    assert_eq!(summary.pages_cleaned, 1);
    assert_eq!(summary.next_page_index, None);
    let stopped = recorder.events().into_iter().filter_map(|event| match event {
        Event::Notice { key, params, .. } if key == "cloud.analysis.run.stopped" => Some(params),
        _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(stopped, vec![serde_json::json!({ "page": 3, "code": "cloud_run_profile_changed" })]);
    // Nothing after page 2 was examined, so a later run queues page 3 again.
    let again = plan(&library, "chapter", &chapter_id, None).unwrap();
    assert!(again.iter().any(|entry| entry.page_index == 3));
}

/* ------------------------------------------------------------------ */
/* Detect, then clean                                                  */
/* ------------------------------------------------------------------ */

/// A page with two blocks of lettering: one inside a balloon at x = 40, one
/// outside any balloon at x = 150, over hatched art. The art is what keeps the
/// second block outside: on flat paper the paper walk would rescue it into a
/// bubble ([`cleaner_core::balloon::in_bubble`]). `n` makes each page's bytes
/// its own, or ingest would deduplicate them.
fn lettered_page(n: u8) -> Raster {
    let mut page = gray_test_page(240, 140);
    for y in 0..140i64 {
        for x in 115..240i64 {
            let block = (150..178).contains(&x) && (55..83).contains(&y);
            if !block && (x + y) % 7 < 2 {
                page.data[(y * 240 + x) as usize] = 150;
            }
        }
    }
    for left in [40i64, 150] {
        for y in 55..83i64 {
            for x in left..left + 28 {
                if (4..24).contains(&(x - left)) && (4..24).contains(&(y - 55)) {
                    page.data[(y * 240 + x) as usize] = 30;
                }
            }
        }
    }
    page.data[0] = 245 - n;
    page
}

fn lettered_detect(crop: &Raster, _: usize) -> cleaner_core::detect::Detection {
    use cleaner_core::detect::{DetBox, DetectedLanguage, Detection, Letterbox, Segmentation};
    let boxes = [40, 150].map(|x| DetBox {
        rect: Rect::new(x, 55, 28, 28),
        confidence: 0.95,
        language: DetectedLanguage::Japanese,
    });
    let mut levels = vec![0; (crop.width * crop.height) as usize];
    for x in [40i64, 150] {
        for y in 55..83i64 {
            for xx in x..x + 28 {
                levels[(y * crop.width as i64 + xx) as usize] = 255;
            }
        }
    }
    Detection {
        boxes: boxes.to_vec(),
        segmentation: Segmentation {
            width: crop.width,
            height: crop.height,
            levels,
            fit: Letterbox::fit(crop.width, crop.height),
        },
    }
}

fn lettered_balloons(_: &Raster, _: usize) -> Vec<cleaner_core::balloon::BalloonBox> {
    vec![cleaner_core::balloon::BalloonBox {
        rect: Rect::new(25, 40, 65, 60),
        class: BalloonClass::TextInBubble,
        score: 0.9,
    }]
}

fn lettered_judge(_: &cleaner_core::detect::Region) -> Verdict {
    Verdict::Clean { script: "Japanese".into() }
}

/// A library of one chapter over `pages` lettered pages.
fn a_lettered_chapter(scratch: &Scratch, pages: usize) -> (Library, String) {
    let library = Library::at(scratch.join("library"));
    let raws = scratch.join("raws");
    std::fs::create_dir_all(&raws).unwrap();
    for n in 1..=pages {
        let bytes = encode(&lettered_page(n as u8), Format::Png).unwrap();
        std::fs::write(raws.join(format!("{n:03}.png")), bytes).unwrap();
    }
    let project =
        library.create_project("Hoshi no Koe", StripMode::Single, Some(raws), None).unwrap();
    let chapter = library.create_chapter(&project.id, "Ch. 1", None, None).unwrap().created().unwrap();
    (library, chapter.id)
}

/// The two picks every test below runs with: bubble text on rung 0, outside
/// text on the inpainter, which this machine's model directory does not have -
/// so one region per page is cleaned and one is declined, by the same ladder.
fn lettered_picks() -> Picks {
    Picks { bubble: EnginePick::Fill, outside: EnginePick::Lama }
}

/// A pipeline whose vision is the lettered page's, over a model directory of
/// empty detection files and no inpainter.
fn lettered_pipeline(scratch: &Scratch) -> Pipeline {
    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap().with_picks(lettered_picks());
    pipeline.test_vision = Some(TestVision {
        detect: lettered_detect,
        balloons: lettered_balloons,
        judge: lettered_judge,
    });
    pipeline
}

/// Detect stores the requested total around SAM for both bubble and outside
/// lettering. The automatic fit's growth is never baked into either baseline.
#[test]
fn detection_padding_is_total_growth_from_sam_for_inside_and_outside_text() {
    use cleaner_core::text_groups::EvidenceFile;
    for padding in [0, 2, 4] {
        let scratch = Scratch::new(&format!("detect-total-padding-{padding}"));
        let (library, chapter_id) = a_lettered_chapter(&scratch, 1);
        let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Detect).unwrap();
        let recorder = Recorder::new();
        let mut pipeline = lettered_pipeline(&scratch).with_mask_padding(padding);
        let detected = execute_as(RunMode::Detect, "run-padding", &chapter_id, &entries,
            &mut pipeline, Engine::Lama, &recorder.cancel, &recorder.sink());
        assert_eq!((detected.regions_detected, detected.errored), (2, 0));
        let job = only_job(&library, &chapter_id);
        assert!(job.project.detections.iter().any(|row| row.inside));
        assert!(job.project.detections.iter().any(|row| !row.inside));
        for row in &job.project.detections {
            let group = row.group.as_ref().unwrap();
            let bytes = std::fs::read(job.sidecar().join(group.evidence_ref.as_ref().unwrap())).unwrap();
            let seed = EvidenceFile::decode(&bytes).unwrap().lettering(group).unwrap();
            let source = &job.project.sources[row.source_idx];
            let expected = seed.dilated(padding, source.w, source.h);
            let found = job.load_detection(&row.id).unwrap().unwrap();
            assert_eq!(found.mask, expected, "inside={} padding={padding}", row.inside);
            assert_eq!(found.ink, expected);
            assert_eq!(job.load_detection_base(&row.id).unwrap().unwrap(), (seed.clone(), seed));
            assert!(row.padding_from_seed);
            assert_eq!(row.padding_px, padding);
            assert_eq!(row.fit.unwrap().thickness, padding);
        }
    }
}

/// A cleaning pipeline over a directory holding **no model at all**. Any
/// detector, balloon detector or gate it tried to open would fail the page.
fn cleaning_only_pipeline(scratch: &Scratch) -> Pipeline {
    Pipeline::open_for_cleaning(&scratch.join("no-models"), Preference::Automatic, None)
        .with_picks(lettered_picks())
}

fn only_job(library: &Library, chapter_id: &str) -> Job {
    Job::open(&library.resolve_chapter(chapter_id).unwrap()).unwrap()
}

/// Detect and then Clean leave the manifest a one-pass Auto run leaves, region
/// for region: the same masks, lettering, pixels, rungs, order, review flags,
/// provenance snapshot and untouched rows. Three things differ, and they are
/// the three the split changes on purpose: the region ids (`-d{n}` rather than
/// `-r{n}`), the time each took, and the execution provider of a rung with no
/// session, which a one-pass run borrows from its detector and a clean, having
/// no detector, reports as `cpu`.
///
/// The clean runs on a pipeline whose model directory is empty, so the same
/// test is the proof that a clean opens no detector.
/// Clean reads the picks it is started with, not the ones saved at
/// detection: Text cleanup shows them for Clean. Detected with LaMa
/// everywhere, which this machine does not have, nothing could be cleaned;
/// cleaned with bubble text on Fill, each page's bubble is.
#[test]
fn a_clean_starts_stored_regions_from_its_own_picks() {
    let scratch = Scratch::new("clean-own-picks");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 2);
    let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Detect).unwrap();
    let recorder = Recorder::new();
    let mut detector = lettered_pipeline(&scratch)
        .with_picks(Picks { bubble: EnginePick::Lama, outside: EnginePick::Lama });
    execute_as(RunMode::Detect, "run-detect", &chapter_id, &entries, &mut detector, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    let job = only_job(&library, &chapter_id);
    assert!(!job.project.detections.is_empty());
    assert!(job.project.detections.iter().all(|row| row.pick == "lama"));

    let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Clean).unwrap();
    let recorder = Recorder::new();
    let mut cleaner = cleaning_only_pipeline(&scratch);
    let summary = execute_as(RunMode::Clean, "run-clean", &chapter_id, &entries, &mut cleaner, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    assert_eq!((summary.regions_cleaned, summary.errored), (2, 0));
}

#[test]
fn detect_then_clean_leaves_what_one_pass_leaves() {
    let one = Scratch::new("one-pass");
    let (one_library, one_chapter) = a_lettered_chapter(&one, 2);
    let entries = plan(&one_library, "chapter", &one_chapter, None).unwrap();
    let recorder = Recorder::new();
    let mut pipeline = lettered_pipeline(&one).with_mask_padding(4);
    let auto = execute_as(RunMode::Auto, "run-1", &one_chapter, &entries, &mut pipeline, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    assert_eq!((auto.regions_cleaned, auto.regions_detected), (2, 0), "{:?} {:?}",
        only_job(&one_library, &one_chapter).project.regions_untouched, auto);

    let two = Scratch::new("two-step");
    let (two_library, two_chapter) = a_lettered_chapter(&two, 2);
    let entries = plan_for(&two_library, "chapter", &two_chapter, None, RunMode::Detect).unwrap();
    assert_eq!(entries.len(), 2);
    let recorder = Recorder::new();
    let mut detector = lettered_pipeline(&two).with_mask_padding(4);
    let detected = execute_as(RunMode::Detect, "run-2", &two_chapter, &entries, &mut detector, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    assert_eq!((detected.regions_cleaned, detected.regions_detected, detected.pages_cleaned), (0, 4, 2));
    assert!(!recorder.kinds().contains(&"region-done"), "a detection is not a cleaned region");
    let job = only_job(&two_library, &two_chapter);
    assert_eq!(job.project.detections.len(), 4);
    assert!(job.project.patches.is_empty() && job.project.regions_untouched.is_empty());
    assert!(job.project.examined.is_empty(), "Detect marks nothing examined");
    let first = &job.project.detections[0];
    assert_eq!(first.id, format!("{}-d0", library::page_id(&two_chapter, 0)));
    assert!(first.inside && first.balloon_color == Some([245; 3]));
    assert_eq!((first.pick.as_str(), first.detector.as_str(), first.script.as_deref()), ("fill", "local", Some("ja")));
    assert_eq!(job.project.detections[1].pick, "lama");
    assert!(!job.project.detections[1].inside && job.project.detections[1].balloon_color.is_none());
    // Every page with detections waiting is Clean's work, and still Auto's.
    assert_eq!(plan_for(&two_library, "chapter", &two_chapter, None, RunMode::Clean).unwrap().len(), 2);
    assert_eq!(plan(&two_library, "chapter", &two_chapter, None).unwrap().len(), 2);

    let entries = plan_for(&two_library, "chapter", &two_chapter, None, RunMode::Clean).unwrap();
    let recorder = Recorder::new();
    let mut cleaner = cleaning_only_pipeline(&two);
    let cleaned = execute_as(RunMode::Clean, "run-3", &two_chapter, &entries, &mut cleaner, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    assert_eq!((cleaned.regions_cleaned, cleaned.errored), (auto.regions_cleaned, 0));
    assert!(cleaner.detector.held.is_none() && cleaner.balloons.held.is_none() && cleaner.gate.held.is_none(),
        "a clean opened a detection model");

    let one_job = only_job(&one_library, &one_chapter);
    let two_job = only_job(&two_library, &two_chapter);
    assert!(two_job.project.detections.is_empty());
    assert_eq!(one_job.project.examined, two_job.project.examined);
    assert_eq!(one_job.project.counters.declined, two_job.project.counters.declined);
    assert_eq!(one_job.project.counters.gate_dropped, two_job.project.counters.gate_dropped);
    let rows = |job: &Job| job.project.regions_untouched.iter()
        .map(|row| (row.source_idx, row.bbox, row.reason.clone())).collect::<Vec<_>>();
    assert_eq!(rows(&one_job), rows(&two_job));
    assert_eq!(one_job.project.patches.len(), two_job.project.patches.len());
    for (a, b) in one_job.project.patches.iter().zip(&two_job.project.patches) {
        assert!(a.id.contains("-r") && b.id.contains("-d"), "{} {}", a.id, b.id);
        assert_eq!((a.source_idx, a.bbox, a.engine, a.order, &a.review_state),
            (b.source_idx, b.bbox, b.engine, b.order, &b.review_state));
        assert_eq!(a.provenance.mask_sha256, b.provenance.mask_sha256);
        assert_eq!(b.provenance.execution_provider, "cpu");
        let snapshot = |record: &PatchRecord| {
            let mut params = record.provenance.params_snapshot.clone();
            params.as_object_mut().unwrap().remove("elapsed_ms");
            params
        };
        assert_eq!(snapshot(a), snapshot(b));
        let (pa, pb) = (one_job.load_patch(a).unwrap(), two_job.load_patch(b).unwrap());
        assert_eq!((pa.mask, pa.ink), (pb.mask, pb.ink));
        assert_eq!(pa.pixels.data, pb.pixels.data);
    }
    // Finished: nothing left for any mode.
    for mode in [RunMode::Auto, RunMode::Detect, RunMode::Clean] {
        assert!(plan_for(&two_library, "chapter", &two_chapter, None, mode).unwrap().is_empty());
    }
}

/// Detect again replaces what it stored and the untouched rows with them, keeps
/// every patch, and does not store text a patch has already cleaned.
#[test]
fn detecting_again_replaces_detections_and_keeps_patches() {
    let scratch = Scratch::new("detect-twice");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 1);
    let page_id = library::page_id(&chapter_id, 0);
    {
        let mut job = only_job(&library, &chapter_id);
        // A hand edit over the balloon's lettering, made before any run.
        let mut patch = a_patch(&format!("{page_id}-h1"), 0, Engine::Fill);
        let bounds = Rect::new(30, 45, 50, 50);
        patch.mask = Mask::filled(bounds);
        patch.ink = Mask::filled(bounds);
        patch.pixels.width = bounds.w;
        patch.pixels.height = bounds.h;
        patch.pixels.data = vec![245; (bounds.w * bounds.h) as usize];
        job.complete_region(0, &patch, None).unwrap();
        job.project.counters.gate_dropped += 1;
        job.leave_untouched(0, Rect::new(200, 10, 8, 8), "review.reason.gateSkippedNotJapanese").unwrap();
    }
    // By page: a chapter Detect leaves a page it detected alone.
    let detect_once = || {
        let entries = plan_for(&library, "page", &chapter_id, Some(0), RunMode::Detect).unwrap();
        let recorder = Recorder::new();
        let mut pipeline = lettered_pipeline(&scratch);
        let summary = execute_as(RunMode::Detect, "run-1", &chapter_id, &entries, &mut pipeline,
            Engine::Lama, &recorder.cancel, &recorder.sink());
        summary
    };

    assert_eq!(detect_once().regions_detected, 1, "the balloon's text is already cleaned");
    let job = only_job(&library, &chapter_id);
    let first = job.project.detections.clone();
    assert_eq!(first.len(), 1);
    assert!(first[0].bbox.x >= 140, "the stored region is the outside one: {:?}", first[0].bbox);
    assert_eq!(job.project.patches.len(), 1, "the hand patch stays");
    assert!(job.project.regions_untouched.is_empty(), "the old rows go");
    assert_eq!(job.project.counters.gate_dropped, 0, "and their counters with them");
    assert!(job.sidecar().join(&first[0].mask_ref).exists());

    assert_eq!(detect_once().regions_detected, 1);
    let job = only_job(&library, &chapter_id);
    assert_eq!(job.project.detections.len(), 1, "a second detect replaced the first");
    assert_eq!(job.project.detections[0].id, format!("{page_id}-d1"));
    assert_eq!(job.project.detections[0].bbox, first[0].bbox);
    assert!(!job.sidecar().join(&first[0].mask_ref).exists(), "the replaced mask file went");
    assert_eq!(job.project.patches.len(), 1);
    assert!(job.project.examined.is_empty());
}

/// The crops [`dark_detect`] was shown, by size.
static AREA_CROPS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

/// Lettering wherever the crop is dark, one box per block: a detector that
/// answers from the pixels it is shown, so a crop cut from anywhere in the
/// page finds the page's own lettering at the crop's own coordinates.
fn dark_detect(crop: &Raster, _: usize) -> cleaner_core::detect::Detection {
    use cleaner_core::detect::{DetBox, DetectedLanguage, Detection, Letterbox, Segmentation};
    AREA_CROPS.lock().unwrap().push((crop.width, crop.height));
    let width = crop.width as usize;
    let mut levels = vec![0; width * crop.height as usize];
    let mut blocks: Vec<(i64, i64, i64, i64)> = Vec::new();
    for (at, _) in crop.data.iter().enumerate().filter(|(_, value)| **value < 60) {
        levels[at] = 255;
        let (x, y) = ((at % width) as i64, (at / width) as i64);
        match blocks.iter_mut().find(|b| x >= b.0 - 8 && x <= b.2 + 8 && y >= b.1 - 8 && y <= b.3 + 8) {
            Some(block) => *block = (block.0.min(x), block.1.min(y), block.2.max(x), block.3.max(y)),
            None => blocks.push((x, y, x, y)),
        }
    }
    Detection {
        boxes: blocks.into_iter().map(|(x0, y0, x1, y1)| DetBox {
            rect: Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32),
            confidence: 0.95,
            language: DetectedLanguage::Japanese,
        }).collect(),
        segmentation: Segmentation {
            width: crop.width,
            height: crop.height,
            levels,
            fit: Letterbox::fit(crop.width, crop.height),
        },
    }
}

/// A Detect held to an area reads a window around it at the page's own
/// resolution, stores what it finds there in page coordinates, and adds it to
/// what the page holds: nothing stored is replaced, nothing is stored twice,
/// and the page is neither detected nor examined by it.
#[test]
fn an_area_detect_adds_what_it_finds_there_and_keeps_the_rest() {
    let scratch = Scratch::new("detect-area");
    let library = Library::at(scratch.join("library"));
    let raws = scratch.join("raws");
    std::fs::create_dir_all(&raws).unwrap();
    // Two blocks of lettering further apart than the models' window is wide.
    let blocks = [(200i64, 300i64), (1300, 2000)];
    let mut page = gray_test_page(1600, 2400);
    for (left, top) in blocks {
        for y in top..top + 40 {
            for x in left..left + 40 {
                page.data[(y * 1600 + x) as usize] = 30;
            }
        }
    }
    std::fs::write(raws.join("001.png"), encode(&page, Format::Png).unwrap()).unwrap();
    let project = library.create_project("Hoshi no Koe", StripMode::Single, Some(raws), None).unwrap();
    let chapter_id = library.create_chapter(&project.id, "Ch. 1", None, None).unwrap().created().unwrap().id;
    let page_id = library::page_id(&chapter_id, 0);

    let models = scratch.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in REQUIRED_MODELS {
        std::fs::write(models.join(name), b"").unwrap();
    }
    // Around a block: a box of 128 px with the block in its middle.
    let around = |(left, top): (i64, i64)| serde_json::json!({
        "x": (left - 44) as f64 / 16.0, "y": (top - 44) as f64 / 24.0, "w": 8.0, "h": 128.0 / 24.0,
    });
    let detect = |area: serde_json::Value| {
        let area = DetectArea::from_arg(Some(&area)).unwrap();
        let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap()
            .with_picks(lettered_picks())
            .with_area(area);
        pipeline.test_vision = Some(TestVision {
            detect: dark_detect,
            balloons: |_, _| Vec::new(),
            judge: lettered_judge,
        });
        let entries = plan_cloud_detection(&library, "page", &chapter_id, Some(0)).unwrap();
        assert_eq!(entries.len(), 1, "a page is always there to look at again");
        let recorder = Recorder::new();
        let summary = execute_as(RunMode::Detect, "run-area", &chapter_id, &entries, &mut pipeline,
            Engine::Lama, &recorder.cancel, &recorder.sink());
        assert_eq!(summary.errored, 0);
        summary.regions_detected
    };

    assert_eq!(detect(around(blocks[1])), 1);
    assert_eq!(AREA_CROPS.lock().unwrap().as_slice(), &[(1024, 1024)], "the models read a window, not the page");
    let job = only_job(&library, &chapter_id);
    let first = job.project.detections.clone();
    assert_eq!(first.len(), 1);
    assert!(first[0].bbox.contains(1320, 2020) && !first[0].bbox.contains(220, 320), "{:?}", first[0].bbox);
    let stored = job.load_detection(&first[0].id).unwrap().unwrap();
    assert!(stored.mask.contains(1320, 2020), "the mask is in page pixels: {:?}", stored.mask.bounds);
    assert!(job.project.detected.is_empty() && job.project.examined.is_empty(), "an area is not the page");

    assert_eq!(detect(around(blocks[1])), 0, "what the page holds is not stored twice");
    assert_eq!(detect(serde_json::json!({ "x": 45.0, "y": 5.0, "w": 8.0, "h": 6.0 })), 0, "bare paper");
    assert_eq!(only_job(&library, &chapter_id).project.detections, first);

    assert_eq!(detect(around(blocks[0])), 1);
    let job = only_job(&library, &chapter_id);
    assert_eq!(job.project.detections.len(), 2);
    assert_eq!(job.project.detections[0], first[0], "the first detection is as it was");
    let second = &job.project.detections[1];
    assert_eq!(second.id, format!("{page_id}-d1"));
    assert!(second.bbox.contains(220, 320), "{:?}", second.bbox);
    assert!(second.order > first[0].order, "after everything on the page");
    assert!(job.sidecar().join(&first[0].mask_ref).exists());
}

/// An area is the seam's `{x, y, w, h}` in percent of the page, refused when
/// it is not on the page, and a run held to one is resumed held to it: as a
/// page Detect it would replace every detection the page holds.
#[test]
fn an_area_is_validated_and_captured_for_resume() {
    for bad in [
        serde_json::json!({ "x": 10, "y": 10, "w": 0, "h": 5 }),
        serde_json::json!({ "x": -1, "y": 10, "w": 5, "h": 5 }),
        serde_json::json!({ "x": 98, "y": 10, "w": 5, "h": 5 }),
        serde_json::json!({ "x": 10, "y": 10, "w": 5 }),
        serde_json::json!("everywhere"),
    ] {
        assert_eq!(DetectArea::from_arg(Some(&bad)), Err("detect_area_invalid".to_owned()), "{bad}");
    }
    assert_eq!(DetectArea::from_arg(None), Ok(None));
    assert_eq!(DetectArea::from_arg(Some(&serde_json::Value::Null)), Ok(None));

    let area = DetectArea::from_arg(Some(&serde_json::json!({ "x": 75, "y": 80, "w": 10, "h": 5 }))).unwrap().unwrap();
    let on_page = area.on_page(1600, 2400);
    assert_eq!(on_page, Rect::new(1200, 1920, 160, 120));
    // Grown about its middle to the window's side, and moved back inside.
    assert_eq!(DetectArea::window(on_page, 1600, 2400), Rect::new(576, 1376, 1024, 1024));
    assert_eq!(DetectArea::window(Rect::new(0, 0, 10, 10), 1600, 2400), Rect::new(0, 0, 1024, 1024));
    assert_eq!(DetectArea::window(on_page, 800, 2400), Rect::new(0, 1376, 800, 1024), "never wider than the page");

    let (_scratch, pipeline) = snapshot_pipeline("area-snapshot");
    let snapshot = run_settings_snapshot(
        &serde_json::json!({}), None, &pipeline.with_area(Some(area)), None, "page", Some(0), RunMode::Detect,
    );
    assert_eq!(captured_run_policy(&snapshot).unwrap().area, Some(area));
    let (_scratch, pipeline) = snapshot_pipeline("area-snapshot-none");
    let snapshot = run_settings_snapshot(
        &serde_json::json!({}), None, &pipeline, None, "page", Some(0), RunMode::Detect,
    );
    assert_eq!(captured_run_policy(&snapshot).unwrap().area, None);
}

#[test]
fn failed_or_cancelled_redetect_keeps_previous_detections_and_files() {
    let scratch = Scratch::new("redetect-preserves");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 1);
    let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Detect).unwrap();
    let recorder = Recorder::new();
    let mut detector = lettered_pipeline(&scratch);
    execute_as(RunMode::Detect, "run-1", &chapter_id, &entries, &mut detector, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    let before = only_job(&library, &chapter_id).project.detections;
    assert!(!before.is_empty());

    struct FailingDetector;
    impl Cleaner for FailingDetector {
        fn clean_page(&mut self, _: &str, _: &[u8], _: Engine, _: &PageContext) -> Result<PageOutcome, String> {
            unreachable!()
        }
        fn detect_page(&mut self, _: &str, _: &[u8], _: Engine, _: &PageContext) -> Result<PageOutcome, String> {
            Err("detector failed".into())
        }
    }
    let recorder = Recorder::new();
    let summary = execute_as(RunMode::Detect, "run-2", &chapter_id, &entries, &mut FailingDetector,
        Engine::Lama, &recorder.cancel, &recorder.sink());
    assert_eq!(summary.errored, 1);
    let job = only_job(&library, &chapter_id);
    assert_eq!(job.project.detections, before);
    for row in &before { assert!(job.load_detection(&row.id).unwrap().is_some()); }

    let recorder = Recorder::tripping_on("page-started");
    let mut detector = lettered_pipeline(&scratch);
    let summary = execute_as(RunMode::Detect, "run-3", &chapter_id, &entries, &mut detector,
        Engine::Lama, &recorder.cancel, &recorder.sink());
    assert_eq!(summary.reason, "cancelled");
    let job = only_job(&library, &chapter_id);
    assert_eq!(job.project.detections, before);
    for row in &before { assert!(job.load_detection(&row.id).unwrap().is_some()); }
}

#[test]
fn auto_and_detect_test_the_detected_box_against_old_patches() {
    struct WideMask;
    impl Cleaner for WideMask {
        fn clean_page(&mut self, page_id: &str, _: &[u8], _: Engine, _: &PageContext) -> Result<PageOutcome, String> {
            let patch = a_patch(&format!("{page_id}-r0"), 0, Engine::Fill);
            let mut outcome = PageOutcome::default();
            outcome.detected_boxes.insert(patch.id.clone(), Rect::new(20, 20, 4, 4));
            outcome.regions.push(RegionOutcome::Cleaned(Box::new(patch), None));
            Ok(outcome)
        }
        fn detect_page(&mut self, _: &str, _: &[u8], _: Engine, _: &PageContext) -> Result<PageOutcome, String> {
            let bbox = Rect::new(20, 20, 4, 4);
            let mut outcome = PageOutcome::default();
            outcome.regions.push(RegionOutcome::Detected(Box::new(LoadedDetection {
                record: cleaner_core::project::DetectedRegion {
                    id: String::new(), source_idx: 0, bbox, mask_ref: String::new(), inside: false,
                    balloon_color: None, script: None, pick: "fill".into(), detector: "local".into(),
                    created: 0, mask_sha256: String::new(), order: 0, review_state: None, fit: None, group: None,
                    padding_from_seed: false,
                    padding_px: 0,
                }, mask: Mask::filled(Rect::new(2, 2, 4, 4)), ink: Mask::filled(bbox), evidence: None,
                base: None,
            })));
            Ok(outcome)
        }
    }
    for mode in [RunMode::Auto, RunMode::Detect] {
        let scratch = Scratch::new("detected-box-covered");
        let (library, _, chapter_id) = a_chapter(&scratch, 1);
        let path = library.resolve_chapter(&chapter_id).unwrap();
        let mut job = Job::open(&path).unwrap();
        let page_id = library::page_id(&chapter_id, 0);
        job.complete_region(0, &a_patch(&format!("{page_id}-h1"), 0, Engine::Fill), None).unwrap();
        let entries = plan_for(&library, "chapter", &chapter_id, None, mode).unwrap();
        let recorder = Recorder::new();
        let summary = execute_as(mode, "run-1", &chapter_id, &entries, &mut WideMask,
            Engine::Fill, &recorder.cancel, &recorder.sink());
        let job = Job::open(&path).unwrap();
        if mode == RunMode::Auto {
            assert_eq!(summary.regions_cleaned, 1);
            assert_eq!(job.project.patches.len(), 2);
        } else {
            assert_eq!(summary.regions_detected, 1);
            assert_eq!(job.project.detections.len(), 1);
        }
    }
}

/// A cancel between two detections of a page keeps the one cleaned and leaves
/// the other waiting, the page unexamined; the next clean finishes it.
#[test]
fn a_cancelled_clean_leaves_the_rest_detected_and_the_next_one_finishes() {
    let scratch = Scratch::new("clean-cancel");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 1);
    let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Detect).unwrap();
    let recorder = Recorder::new();
    let mut pipeline = lettered_pipeline(&scratch);
    execute_as(RunMode::Detect, "run-1", &chapter_id, &entries, &mut pipeline, Engine::Lama,
        &recorder.cancel, &recorder.sink());

    let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Clean).unwrap();
    let recorder = Recorder::tripping_on("region-done");
    let mut cleaner = cleaning_only_pipeline(&scratch);
    let summary = execute_as(RunMode::Clean, "run-2", &chapter_id, &entries, &mut cleaner, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    assert_eq!((summary.reason, summary.regions_cleaned, summary.next_page_index), ("cancelled", 1, Some(0)));
    let job = only_job(&library, &chapter_id);
    assert_eq!((job.project.patches.len(), job.project.detections.len()), (1, 1));
    assert!(job.project.examined.is_empty());
    assert_eq!(job.project.interrupted_at, Some(0));

    let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Auto).unwrap();
    assert_eq!(entries.len(), 1, "Auto takes the page with a detection waiting");
    assert!(pages_to_detect(&entries, RunMode::Auto).is_empty(), "and would send it to no detector");
    let recorder = Recorder::new();
    let mut cleaner = cleaning_only_pipeline(&scratch);
    let summary = execute_as(RunMode::Auto, "run-3", &chapter_id, &entries, &mut cleaner, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    assert_eq!((summary.reason, summary.errored), ("completed", 0));
    let job = only_job(&library, &chapter_id);
    assert!(job.project.detections.is_empty());
    assert_eq!(job.project.regions_untouched.len(), 1, "the outside text declined for want of rung 2");
    assert_eq!(job.project.counters.declined, 1);
    assert_eq!(job.project.examined, vec![0]);
}

/// Whether anything is left to clean decides a Clean's queue, and a chapter
/// nobody detected on has none - the case `run_clean` answers with
/// `notice.run.nothingDetected`.
#[test]
fn a_chapter_nothing_detected_on_has_no_clean_to_run() {
    let scratch = Scratch::new("nothing-detected");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 2);
    assert!(plan_for(&library, "chapter", &chapter_id, None, RunMode::Clean).unwrap().is_empty());
    assert_eq!(plan_for(&library, "chapter", &chapter_id, None, RunMode::Detect).unwrap().len(), 2);
}

/// A Detect that stopped after its first page picks up at the second: the
/// page it finished is recorded detected, and only a run that names it by
/// page detects it again. Before the record a stopped Detect, cloud runs
/// included, started the chapter over on every retry.
#[test]
fn a_stopped_detect_resumes_where_it_stopped() {
    let scratch = Scratch::new("detect-resume");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 2);
    let entries = plan_for(&library, "chapter", &chapter_id, None, RunMode::Detect).unwrap();
    let recorder = Recorder::tripping_on("page-done");
    let mut pipeline = lettered_pipeline(&scratch);
    let summary = execute_as(RunMode::Detect, "run-1", &chapter_id, &entries, &mut pipeline, Engine::Lama,
        &recorder.cancel, &recorder.sink());
    assert_eq!(summary.reason, "cancelled");
    let job = only_job(&library, &chapter_id);
    assert_eq!(job.project.detected, vec![0]);
    assert!(job.project.examined.is_empty(), "text was found, so the page waits for a clean");

    let again: Vec<usize> = plan_for(&library, "chapter", &chapter_id, None, RunMode::Detect).unwrap()
        .iter().map(|entry| entry.page_index).collect();
    assert_eq!(again, vec![1], "the detected page is not detected again");
    assert_eq!(plan_for(&library, "page", &chapter_id, Some(0), RunMode::Detect).unwrap().len(), 1,
        "a page asked for by name is");
    assert_eq!(plan_for(&library, "chapter", &chapter_id, None, RunMode::Clean).unwrap().len(), 1);
}

/// A resumed run is the mode it was started as. A snapshot from before the
/// split resumes as the one-pass run it was, and a mode this build does not
/// know is refused rather than run as something else.
#[test]
fn an_interrupted_run_resumes_in_its_own_mode() {
    let (_scratch, pipeline) = snapshot_pipeline("resume-mode");
    for mode in [RunMode::Auto, RunMode::Detect, RunMode::Clean] {
        let snapshot = run_settings_snapshot(
            &serde_json::json!({}), None, &pipeline, None, "chapter", None, mode,
        );
        assert_eq!(snapshot["runMode"], mode.as_str());
        assert_eq!(captured_run_policy(&snapshot).unwrap().mode, mode);
    }
    assert_eq!(captured_run_policy(&serde_json::json!({})).unwrap().mode, RunMode::Auto);
    assert!(captured_run_policy(&serde_json::json!({ "runMode": "sideways" })).is_err());
    assert_eq!(RunMode::from_arg(None), Ok(RunMode::Auto));
    assert!(RunMode::from_arg(Some("Detect")).is_err());
}

/// A stored `solid` pick is filled with the balloon's own measured colour, not
/// the Clean run's `bubbleColor`: the interface always sends one, white by
/// default, and the setting first would paint every grey balloon white. A
/// detection outside a balloon measured none and takes the run's colour.
#[test]
fn a_solid_pick_fills_with_its_measured_balloon_colour_before_the_runs() {
    let page = gray_test_page(120, 80);
    let bytes = encode(&page, Format::Png).unwrap();
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let survey = cleaner_core::strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip, joins: &survey.joins, segments: &survey.segments, placement: 0, sources: &[],
    };
    let stored = |id: &str, bbox: Rect, balloon_color: Option<[u8; 3]>| LoadedDetection {
        record: cleaner_core::project::DetectedRegion {
            id: id.into(), source_idx: 0, bbox, mask_ref: String::new(), inside: balloon_color.is_some(),
            balloon_color, script: None, pick: "solid".into(), detector: "local".into(), created: 0,
            mask_sha256: String::new(), order: 0, review_state: None, fit: None, group: None,
            padding_from_seed: false,
            padding_px: 0,
        },
        mask: Mask::filled(bbox),
        ink: Mask::filled(bbox),
        evidence: None,
        base: None,
    };
    let measured = Rect::new(20, 20, 16, 12);
    let outside = Rect::new(80, 40, 16, 12);
    let mut pipeline = Pipeline::open_for_cleaning(Path::new(""), Preference::default(), None)
        .with_bubble_color(Some([255, 255, 255]));
    let outcome = pipeline.clean_detections("c1-p001", &bytes, Engine::Fill, &context, &[
        stored("c1-p001-d0", measured, Some([180, 180, 180])),
        stored("c1-p001-d1", outside, None),
    ]).unwrap();
    let fill_at = |id: &str, bbox: Rect| {
        let (_, region) = outcome.regions.iter().find(|(region_id, _)| region_id == id).unwrap();
        let RegionOutcome::Cleaned(patch, _) = region else { panic!("{id}: {}", one_line(region)) };
        let (x, y) = (bbox.x + bbox.w as i64 / 2 - patch.mask.bounds.x, bbox.y + bbox.h as i64 / 2 - patch.mask.bounds.y);
        let channels = (patch.pixels.data.len() / (patch.pixels.width * patch.pixels.height) as usize).max(1);
        patch.pixels.data[(y as usize * patch.pixels.width as usize + x as usize) * channels]
    };
    assert_eq!(fill_at("c1-p001-d0", measured), 180);
    assert_eq!(fill_at("c1-p001-d1", outside), 255);
}

/// A one-pass run on a page whose detections were cleaned (or deleted) and
/// that was never examined does not clean the same text a second time: a
/// region half inside a patch the page already holds is left to that patch.
#[test]
fn an_auto_pass_leaves_regions_inside_the_pages_existing_patches_alone() {
    let scratch = Scratch::new("auto-kept");
    let (library, _, chapter_id) = a_chapter(&scratch, 2);
    let entries = plan(&library, "chapter", &chapter_id, None).unwrap();
    let first_page = library::page_id(&chapter_id, 0);
    {
        let mut job = Job::open(&entries[0].job_path).unwrap();
        // A cleaned detection over the very box the stub will find.
        job.complete_region(entries[0].source_idx, &a_patch(&format!("{first_page}-d0"), 0, Engine::Fill), None)
            .unwrap();
    }
    let recorder = Recorder::new();
    let summary = execute("run-1", &chapter_id, &entries, &mut Stub::new(1, 0), Engine::Flux, &recorder.cancel,
        &recorder.sink());

    assert_eq!(summary.reason, "completed");
    assert_eq!(summary.regions_cleaned, 1, "only the page without a patch there is cleaned");
    let job = Job::open(&entries[0].job_path).unwrap();
    let ids: Vec<&str> = job.project.patches.iter().map(|record| record.id.as_str()).collect();
    assert!(ids.contains(&format!("{first_page}-d0").as_str()));
    assert!(!ids.contains(&format!("{first_page}-r0").as_str()), "cleaned twice: {ids:?}");
    assert!(ids.contains(&format!("{}-r0", library::page_id(&chapter_id, 1)).as_str()));
}

/* ------------------------------------------------------------------ */
/* Text groups through the run                                          */
/* ------------------------------------------------------------------ */

mod grouped {
    use super::*;
    use cleaner_core::balloon::{BalloonBox, BalloonClass};
    use cleaner_core::detect::{Detection, Letterbox, Region, Segmentation};
    use cleaner_core::text_groups::{EvidenceFile, GroupOrigin};

    /// Glyphs are dark rings on light paper. Balloon A holds a vertical column,
    /// a glyph in two strokes and a period; balloon B a horizontal row. A dark
    /// disk with no box is artwork.
    fn glyph(page: &mut Raster, x: i64, y: i64, s: i64) {
        for yy in y..y + s {
            for xx in x..x + s {
                let inner = (xx - x) >= s / 3 && (xx - x) < s - s / 3 && (yy - y) >= s / 3 && (yy - y) < s - s / 3;
                if !inner {
                    page.data[(yy * page.width as i64 + xx) as usize] = 30;
                }
            }
        }
    }

    fn paint(page: &mut Raster, rect: Rect) {
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                page.data[(y * page.width as i64 + x) as usize] = 30;
            }
        }
    }

    const ART: (i64, i64) = (340, 190);

    fn grouped_page() -> Raster {
        let mut page = gray_test_page(400, 400);
        for y in [40, 64, 88, 112] {
            glyph(&mut page, 100, y, 18);
        }
        paint(&mut page, Rect::new(100, 136, 5, 18));
        paint(&mut page, Rect::new(112, 136, 6, 18));
        paint(&mut page, Rect::new(113, 160, 4, 4));
        for x in [190, 215, 240] {
            glyph(&mut page, x, 50, 18);
        }
        for y in ART.1 - 20..=ART.1 + 20 {
            for x in ART.0 - 20..=ART.0 + 20 {
                if (x - ART.0).pow(2) + (y - ART.1).pow(2) <= 400 {
                    page.data[(y * 400 + x) as usize] = 30;
                }
            }
        }
        page
    }

    fn detect(crop: &Raster, _: usize) -> Detection {
        Detection { boxes: Vec::new(), segmentation: Segmentation {
            width: crop.width, height: crop.height,
            levels: vec![0; (crop.width * crop.height) as usize],
            fit: Letterbox::fit(crop.width, crop.height),
        } }
    }

    fn balloons(_: &Raster, _: usize) -> Vec<BalloonBox> {
        vec![
            BalloonBox { rect: Rect::new(20, 20, 150, 200), class: BalloonClass::Bubble, score: 0.9 },
            BalloonBox { rect: Rect::new(96, 36, 26, 122), class: BalloonClass::TextInBubble, score: 0.9 },
            BalloonBox { rect: Rect::new(175, 20, 120, 100), class: BalloonClass::Bubble, score: 0.9 },
            BalloonBox { rect: Rect::new(186, 46, 76, 26), class: BalloonClass::TextInBubble, score: 0.9 },
        ]
    }

    fn judge(_: &Region) -> Verdict {
        Verdict::Clean { script: "Japanese".into() }
    }

    /// SAM-TS-L's stand-in: every dark pixel is lettering, artwork included.
    fn lettering(crop: &Raster, _: usize) -> Vec<u8> {
        (0..crop.height).flat_map(|y| (0..crop.width).map(move |x| (x, y)))
            .map(|(x, y)| if crop.luma16_at(x, y) >> 8 < 128 { 255 } else { 0 })
            .collect()
    }

    fn pipeline(scratch: &Scratch) -> Pipeline {
        let models = scratch.join("models");
        std::fs::create_dir_all(&models).unwrap();
        for name in REQUIRED_MODELS {
            std::fs::write(models.join(name), b"").unwrap();
        }
        let mut pipeline = Pipeline::open(&models, Preference::Automatic).unwrap()
            .with_picks(Picks { bubble: EnginePick::Fill, outside: EnginePick::Fill });
        pipeline.test_vision = Some(TestVision { detect, balloons, judge });
        pipeline.test_lettering = Some(lettering);
        pipeline.detection_models = DetectorModels { ctd: false, rt_small: false, rt_full: true, sam: true };
        pipeline
    }

    fn single_page_context(strip: &Strip, survey: &Survey) -> PageContext<'static> {
        let strip: &'static Strip = Box::leak(Box::new(strip.clone()));
        let survey: &'static Survey = Box::leak(Box::new(survey.clone()));
        PageContext { strip, joins: &survey.joins, segments: &survey.segments, placement: 0, sources: &[] }
    }

    /// Many disconnected glyphs become one job per text group, never one per
    /// component; lettering no box claims (here artwork the stand-in mask
    /// calls lettering) is its own job, never part of a boxed group, with the
    /// reason in its record; and each stored group traces back to the exact
    /// mask evidence it came from.
    #[test]
    fn many_glyphs_become_one_job_per_group_with_traceable_evidence() {
        let scratch = Scratch::new("grouped-detect");
        let mut pipeline = pipeline(&scratch);
        let page = grouped_page();
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = single_page_context(&strip, &survey);

        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let detected: Vec<&LoadedDetection> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Detected(found) => Some(found.as_ref()),
            _ => None,
        }).collect();
        assert_eq!(detected.len(), 3, "{:?}", outcome.regions.iter().map(one_line).collect::<Vec<_>>());
        assert!(!outcome.regions.iter().any(|region| matches!(region,
            RegionOutcome::Candidate { .. } | RegionOutcome::Untouched { .. })));
        let (unboxed, boxed): (Vec<&LoadedDetection>, Vec<&LoadedDetection>) = detected.iter()
            .partition(|found| found.record.group.as_ref().unwrap().origin == GroupOrigin::MaskLayout);
        assert_eq!(unboxed.len(), 1);
        assert!(unboxed[0].mask.contains(ART.0, ART.1));
        // Was `!inside`: no balloon box holds it, but it sits on flat paper, and
        // `in_bubble` lets the page rescue what the detector left outside.
        assert!(unboxed[0].record.inside, "unboxed lettering on flat paper is read as in a bubble");
        assert_eq!(unboxed[0].record.review_state, None, "provenance, not a review flag");
        assert_eq!(unboxed[0].record.group.as_ref().unwrap().reasons,
            vec![cleaner_core::text_groups::ReviewReason::UnassignedComponent]);

        let mut components = Vec::new();
        for found in &boxed {
            let group = found.record.group.as_ref().expect("a grouped detection records its group");
            assert_eq!(group.origin, GroupOrigin::TextBox);
            assert_eq!(found.record.review_state, None, "an unflagged group is not a problem");
            assert!(!found.mask.contains(ART.0, ART.1) && !found.ink.contains(ART.0, ART.1));
            let blob = found.evidence.as_ref().expect("the evidence file travels with the detection");
            assert_eq!(group.evidence_sha256.as_deref(), Some(blob.sha256.as_str()));
            let file = EvidenceFile::decode(&blob.bytes).unwrap();
            assert_eq!(file.grouping.boxes.len(), 4, "raw boxes, bubbles included, are kept");
            let rebuilt = file.lettering(group).unwrap();
            assert_eq!(sha256_hex(&buffers::encode_mask(&rebuilt)), group.lettering_sha256);
            assert_eq!(rebuilt.count() as u32, group.lettering_pixels);
            components.push(group.components.len());
        }
        components.sort_unstable();
        assert_eq!(components, vec![3, 7], "the column, its two strokes and the period are one group");

        // One pass cleans the same three groups, the artwork only in its own.
        let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let cleaned: Vec<&Patch> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Cleaned(patch, _) => Some(patch.as_ref()),
            _ => None,
        }).collect();
        assert_eq!(cleaned.len(), 3);
        assert_eq!(cleaned.iter().filter(|patch| patch.mask.contains(ART.0, ART.1)).count(), 1);
        for patch in cleaned {
            let group = &patch.provenance.params_snapshot["group"];
            assert!(group["id"].as_str().is_some_and(|id| id.starts_with("tg-")), "{group}");
        }
    }

    /// A group in two segments' detection overlap is one job, and so is
    /// unboxed lettering there.
    #[test]
    fn a_group_in_the_segment_overlap_is_one_job() {
        fn tall_balloons(_: &Raster, segment: usize) -> Vec<BalloonBox> {
            let top = if segment == 0 { 2_300 } else { 300 };
            vec![BalloonBox { rect: Rect::new(96, top, 26, 80), class: BalloonClass::TextFree, score: 0.9 }]
        }
        let scratch = Scratch::new("grouped-seam");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: tall_balloons, judge });
        let mut page = gray_test_page(400, 4_000);
        for y in [2_305, 2_329, 2_353] {
            glyph(&mut page, 100, y, 18);
        }
        paint(&mut page, Rect::new(300, 2_500, 30, 30));
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let splits = [Split { y: 2_000, kind: SplitKind::Fallback }];
        let segments = cleaner_core::strip::detection_segments(4_000, &splits);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].detect_end > 2_600, "the group and the art are in the overlap");
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = PageContext {
            strip: &strip, joins: &survey.joins, segments: &segments, placement: 0, sources: &[],
        };
        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let detected = outcome.regions.iter().filter(|r| matches!(r, RegionOutcome::Detected(..))).count();
        let held = outcome.regions.iter().filter(|r| matches!(r, RegionOutcome::Candidate { .. })).count();
        assert_eq!((detected, held), (2, 0), "{:?}", outcome.regions.iter().map(one_line).collect::<Vec<_>>());
    }

    /// Unboxed lettering one segment sees across the cut and a region the next
    /// segment boxes over part of it are one region: the column is cleaned
    /// whole, not cut at the split.
    #[test]
    fn unboxed_lettering_above_the_cut_and_the_next_segments_region_are_one() {
        fn second_segment_boxes(_: &Raster, segment: usize) -> Vec<BalloonBox> {
            // Only the second segment's detector boxes the column.
            if segment == 0 {
                Vec::new()
            } else {
                vec![BalloonBox { rect: Rect::new(96, 0, 26, 80), class: BalloonClass::TextFree, score: 0.9 }]
            }
        }
        let scratch = Scratch::new("grouped-candidate-owner");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: second_segment_boxes, judge });
        let mut page = gray_test_page(400, 4_000);
        // A column whose first glyph is above the cut: the first segment sees
        // all four as one unboxed group starting in its own rows.
        for y in [1_980, 2_004, 2_028, 2_052] {
            glyph(&mut page, 100, y, 18);
        }
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let splits = [Split { y: 2_000, kind: SplitKind::Fallback }];
        let segments = cleaner_core::strip::detection_segments(4_000, &splits);
        assert_eq!((segments.len(), segments[1].start), (2, 2_000));
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = PageContext {
            strip: &strip, joins: &survey.joins, segments: &segments, placement: 0, sources: &[],
        };
        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        let detected: Vec<&LoadedDetection> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Detected(found) => Some(found.as_ref()),
            _ => None,
        }).collect();
        assert_eq!((detected.len(), outcome.regions.len()), (1, 1), "{lines:?}");
        assert_eq!(detected[0].record.group.as_ref().unwrap().components.len(), 4);
        assert!(detected[0].mask.contains(105, 1_985), "the glyph above the cut is the region's");
    }

    /// A region only the first segment boxes, in its detection overlap, is
    /// the second segment's by ownership. The second segment's detector does
    /// not box it (it sees the glyphs, unboxed): it is written once, from the
    /// first segment's crop, and not listed as a candidate as well.
    #[test]
    fn a_region_only_the_segment_before_its_owner_boxes_is_written_once() {
        fn first_segment_boxes(_: &Raster, segment: usize) -> Vec<BalloonBox> {
            if segment == 0 {
                vec![BalloonBox { rect: Rect::new(96, 2_300, 26, 80), class: BalloonClass::TextFree, score: 0.9 }]
            } else {
                Vec::new()
            }
        }
        let scratch = Scratch::new("grouped-deferred-region");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: first_segment_boxes, judge });
        let mut page = gray_test_page(400, 4_000);
        for y in [2_305, 2_329, 2_353] {
            glyph(&mut page, 100, y, 18);
        }
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let splits = [Split { y: 2_000, kind: SplitKind::Fallback }];
        let segments = cleaner_core::strip::detection_segments(4_000, &splits);
        assert!(segments[0].detect_end > 2_400, "the run is in the first segment's overlap");
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = PageContext {
            strip: &strip, joins: &survey.joins, segments: &segments, placement: 0, sources: &[],
        };
        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        let detected: Vec<&LoadedDetection> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Detected(found) => Some(found.as_ref()),
            _ => None,
        }).collect();
        assert_eq!(detected.len(), 1, "lost at the cut: {lines:?}");
        let group = detected[0].record.group.as_ref().unwrap();
        assert_eq!(group.components.len(), 3);
        let bbox = detected[0].record.bbox;
        assert!((2_290..=2_305).contains(&bbox.y) && (2_371..=2_390).contains(&bbox.bottom()), "{lines:?}");
        let file = EvidenceFile::decode(&detected[0].evidence.as_ref().unwrap().bytes).unwrap();
        assert_eq!(sha256_hex(&buffers::encode_mask(&file.lettering(group).unwrap())), group.lettering_sha256);
        assert_eq!(outcome.regions.len(), 1, "the glyphs are listed again: {lines:?}");

        // One pass cleans it once too.
        let outcome = pipeline.clean_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        let cleaned = outcome.regions.iter().filter(|r| matches!(r, RegionOutcome::Cleaned(..))).count();
        assert_eq!((cleaned, outcome.regions.len()), (1, 1), "{lines:?}");
    }

    /// Two segments before its owner both box a region and the owner does
    /// not: it is written once, from the later of the two crops.
    #[test]
    fn a_region_two_segments_before_its_owner_box_is_written_once() {
        // Boxes are in crop coordinates; the second segment's crop starts at
        // row 2 000.
        fn early_segments_box(_: &Raster, segment: usize) -> Vec<BalloonBox> {
            match segment {
                0 => vec![BalloonBox { rect: Rect::new(96, 2_500, 26, 80), class: BalloonClass::TextFree, score: 0.9 }],
                1 => vec![BalloonBox { rect: Rect::new(96, 500, 26, 80), class: BalloonClass::TextFree, score: 0.9 }],
                _ => Vec::new(),
            }
        }
        let scratch = Scratch::new("grouped-deferred-twice");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: early_segments_box, judge });
        let mut page = gray_test_page(400, 4_000);
        for y in [2_505, 2_529, 2_553] {
            glyph(&mut page, 100, y, 18);
        }
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let splits = [Split { y: 2_000, kind: SplitKind::Fallback }, Split { y: 2_400, kind: SplitKind::Fallback }];
        let segments = cleaner_core::strip::detection_segments(4_000, &splits);
        assert_eq!(segments.len(), 3);
        assert!(segments[0].detect_end > 2_600 && segments[2].start <= 2_500, "the run is in both earlier overlaps");
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = PageContext {
            strip: &strip, joins: &survey.joins, segments: &segments, placement: 0, sources: &[],
        };
        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        assert_eq!(outcome.regions.len(), 1, "{lines:?}");
        let RegionOutcome::Detected(found) = &outcome.regions[0] else { panic!("{lines:?}") };
        assert_eq!(found.record.group.as_ref().unwrap().components.len(), 3, "{lines:?}");
    }

    /// Under the All text policy the gate never runs, and a region still gets
    /// the real in/out answer: the stored detection and the engine pick read
    /// [`cleaner_core::balloon::in_bubble`], not the detector alone. With no
    /// balloon box anywhere, the block on flat paper is inside only because
    /// the paper rescued it, and the block over hatched art stays outside.
    #[test]
    fn an_all_text_run_still_gets_the_real_inside_answer() {
        fn no_boxes(_: &Raster, _: usize) -> Vec<BalloonBox> {
            Vec::new()
        }
        let scratch = Scratch::new("all-text-inside");
        let mut pipeline = lettered_pipeline(&scratch).with_outside(OutsideText::Clean);
        pipeline.all_text = true;
        pipeline.test_vision = Some(TestVision { detect: lettered_detect, balloons: no_boxes, judge: lettered_judge });
        let page = lettered_page(1);
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = single_page_context(&strip, &survey);

        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Lama, &context).unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        let found: Vec<&LoadedDetection> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Detected(found) => Some(found.as_ref()),
            _ => None,
        }).collect();
        assert_eq!(found.len(), 2, "{lines:?}");
        let at = |x: i64| found.iter().find(|f| f.record.bbox.x <= x && x < f.record.bbox.right()).unwrap();
        let (paper, art) = (at(50), at(160));
        assert!(paper.record.inside, "flat paper, no box: the page's rescue {lines:?}");
        assert_eq!(paper.record.pick, "fill", "and the bubble pick with it");
        assert!(!art.record.inside, "art, no box: outside {lines:?}");
        assert_eq!(art.record.pick, "lama");
    }

    /// A held candidate's `inside_bubble` is the answer a cleaning region
    /// gets ([`candidate_in_bubble`], not the grouping's balloon confinement):
    /// two weak `text_free` boxes with no lettering under them, one on flat
    /// paper and one on hatched art. The detector says outside for both; the
    /// paper rescues the first and not the second.
    #[test]
    fn a_held_candidate_is_inside_by_the_same_answer_a_region_gets() {
        fn weak_boxes(_: &Raster, _: usize) -> Vec<BalloonBox> {
            vec![
                BalloonBox { rect: Rect::new(60, 60, 60, 30), class: BalloonClass::TextFree, score: 0.4 },
                BalloonBox { rect: Rect::new(260, 260, 60, 30), class: BalloonClass::TextFree, score: 0.4 },
            ]
        }
        let scratch = Scratch::new("grouped-candidate-inside");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: weak_boxes, judge });
        let mut page = gray_test_page(400, 400);
        // Hatching light enough that the stand-in mask does not call it
        // lettering, and busy enough that the paper walk reads picture.
        for y in 200..400i64 {
            for x in 200..400i64 {
                if (x + y) % 7 < 2 {
                    page.data[(y * 400 + x) as usize] = 150;
                }
            }
        }
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = single_page_context(&strip, &survey);

        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        let held: Vec<(Rect, bool)> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Candidate { bbox, inside_bubble, .. } => Some((*bbox, *inside_bubble)),
            _ => None,
        }).collect();
        assert_eq!(held.len(), 2, "{lines:?}");
        let at = |x: i64| held.iter().find(|(bbox, _)| bbox.x <= x && x < bbox.right()).unwrap().1;
        assert!(at(90), "flat paper around a weak box: the page's rescue {lines:?}");
        assert!(!at(290), "art around it: outside, as the detector said {lines:?}");
    }

    /// Unboxed lettering only the first segment sees, in its detection
    /// overlap, is written once, from that segment, when the second segment's
    /// evidence misses it.
    #[test]
    fn unboxed_lettering_only_the_segment_before_its_owner_sees_is_written_once() {
        fn first_segment_lettering(crop: &Raster, segment: usize) -> Vec<u8> {
            let mask = lettering(crop, segment);
            if segment == 0 { mask } else { vec![0; mask.len()] }
        }
        fn no_boxes(_: &Raster, _: usize) -> Vec<BalloonBox> {
            Vec::new()
        }
        let scratch = Scratch::new("grouped-deferred-candidate");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: no_boxes, judge });
        pipeline.test_lettering = Some(first_segment_lettering);
        let mut page = gray_test_page(400, 4_000);
        paint(&mut page, Rect::new(300, 2_500, 30, 30));
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let splits = [Split { y: 2_000, kind: SplitKind::Fallback }];
        let segments = cleaner_core::strip::detection_segments(4_000, &splits);
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = PageContext {
            strip: &strip, joins: &survey.joins, segments: &segments, placement: 0, sources: &[],
        };
        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        let detected: Vec<&LoadedDetection> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Detected(found) => Some(found.as_ref()),
            _ => None,
        }).collect();
        assert_eq!((detected.len(), outcome.regions.len()), (1, 1), "{lines:?}");
        assert!(detected[0].mask.contains(310, 2_510), "{lines:?}");
    }

    /// Across a verified join the last segment of a page reads into the next
    /// page, and what it boxes there is that page's by ownership. When the
    /// next page's own detector does not box it, the next page writes it from
    /// the earlier crop, in its own coordinates, with an evidence file that
    /// rebuilds it there.
    #[test]
    fn a_region_seen_across_a_verified_join_is_written_by_the_page_it_is_on() {
        fn first_page_boxes(_: &Raster, segment: usize) -> Vec<BalloonBox> {
            if segment == 0 {
                vec![BalloonBox { rect: Rect::new(96, 2_300, 26, 80), class: BalloonClass::TextFree, score: 0.9 }]
            } else {
                Vec::new()
            }
        }
        let scratch = Scratch::new("grouped-deferred-join");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: first_page_boxes, judge });
        let first = gray_test_page(400, 2_000);
        let mut second = gray_test_page(400, 2_000);
        for y in [305, 329, 353] {
            glyph(&mut second, 100, y, 18);
        }
        let sources: Vec<Option<PathBuf>> = [&first, &second].iter().enumerate().map(|(n, page)| {
            let path = scratch.join(&format!("{n}.png"));
            std::fs::write(&path, encode(page, Format::Png).unwrap()).unwrap();
            Some(path)
        }).collect();
        let strip = Strip::of_sizes(&[(400, 2_000), (400, 2_000)]);
        let mut joins = cleaner_core::strip::Joins::unchecked(1);
        joins.set(0, cleaner_core::strip::JoinState::Verified);
        let segments = cleaner_core::strip::detection_segments(4_000, &[Split { y: 2_000, kind: SplitKind::Join }]);
        assert!(segments[0].detect_end >= 2_900, "the first page's segment reads into the second");
        let context = |placement| PageContext {
            strip: &strip, joins: &joins, segments: &segments, placement, sources: &sources,
        };

        let outcome = pipeline.detect_page("c1-p001", &encode(&first, Format::Png).unwrap(), Engine::Fill, &context(0))
            .unwrap();
        assert!(outcome.regions.is_empty(), "{:?}", outcome.regions.iter().map(one_line).collect::<Vec<_>>());
        let outcome = pipeline.detect_page("c1-p002", &encode(&second, Format::Png).unwrap(), Engine::Fill, &context(1))
            .unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        let detected: Vec<&LoadedDetection> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Detected(found) => Some(found.as_ref()),
            _ => None,
        }).collect();
        assert_eq!((detected.len(), outcome.regions.len()), (1, 1), "{lines:?}");
        let found = detected[0];
        let bbox = found.record.bbox;
        assert!((290..=305).contains(&bbox.y) && (371..=390).contains(&bbox.bottom()), "page coordinates: {lines:?}");
        assert!(found.mask.contains(101, 306) && found.ink.contains(101, 306), "the masks are on this page");
        let group = found.record.group.as_ref().unwrap();
        assert_eq!(group.components.len(), 3);
        assert!(group.bounds.y >= 300 && group.bounds.bottom() <= 380, "{:?}", group.bounds);
        let blob = found.evidence.as_ref().unwrap();
        assert_eq!(group.evidence_sha256.as_deref(), Some(blob.sha256.as_str()));
        let file = EvidenceFile::decode(&blob.bytes).unwrap();
        let rebuilt = file.lettering(group).unwrap();
        assert_eq!(sha256_hex(&buffers::encode_mask(&rebuilt)), group.lettering_sha256);
        assert!(rebuilt.contains(101, 306), "the evidence rebuilds the group on its own page");
    }

    /// The clipped view of a region the previous segment owns is not listed
    /// again as a candidate by the next.
    #[test]
    fn a_clipped_view_of_an_owned_region_is_not_listed_again() {
        fn first_segment_boxes(_: &Raster, segment: usize) -> Vec<BalloonBox> {
            if segment == 0 {
                vec![BalloonBox { rect: Rect::new(96, 1_976, 26, 100), class: BalloonClass::TextFree, score: 0.9 }]
            } else {
                Vec::new()
            }
        }
        let scratch = Scratch::new("grouped-clipped-candidate");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: first_segment_boxes, judge });
        let mut page = gray_test_page(400, 4_000);
        for y in [1_980, 2_004, 2_028, 2_052] {
            glyph(&mut page, 100, y, 18);
        }
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let splits = [Split { y: 2_000, kind: SplitKind::Fallback }];
        let segments = cleaner_core::strip::detection_segments(4_000, &splits);
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = PageContext {
            strip: &strip, joins: &survey.joins, segments: &segments, placement: 0, sources: &[],
        };
        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let lines = outcome.regions.iter().map(one_line).collect::<Vec<_>>();
        let detected = outcome.regions.iter().filter(|r| matches!(r, RegionOutcome::Detected(..))).count();
        let held = outcome.regions.iter().filter(|r| matches!(r, RegionOutcome::Untouched { .. })).count();
        assert_eq!((detected, held), (1, 0), "{lines:?}");
    }

    /// Detect writes each page's evidence file beside the manifest, and the
    /// stored group names it and survives a reopen.
    #[test]
    fn a_detect_run_persists_group_records_and_their_evidence_file() {
        let _exclusive = exclusively();
        let scratch = Scratch::new("grouped-persist");
        let library = Library::at(scratch.join("library"));
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        std::fs::write(raws.join("001.png"), encode(&grouped_page(), Format::Png).unwrap()).unwrap();
        let project = library.create_project("Groups", StripMode::Single, Some(raws), None).unwrap();
        let chapter = library.create_chapter(&project.id, "Ch. 1", None, None).unwrap().created().unwrap();
        let entries = plan_for(&library, "chapter", &chapter.id, None, RunMode::Detect).unwrap();
        let recorder = Recorder::new();
        let mut pipeline = pipeline(&scratch);
        let summary = execute_as(RunMode::Detect, "run-1", &chapter.id, &entries, &mut pipeline,
            Engine::Fill, &recorder.cancel, &recorder.sink());
        assert_eq!((summary.regions_detected, summary.errored), (3, 0));
        let job = Job::open(&entries[0].job_path).unwrap();
        assert_eq!(job.project.detections.len(), 3);
        assert!(job.project.regions_untouched.is_empty());
        for row in &job.project.detections {
            let group = row.group.as_ref().unwrap();
            let path = job.sidecar().join(group.evidence_ref.as_deref().unwrap());
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(sha256_hex(&bytes), *group.evidence_sha256.as_ref().unwrap());
            let file = EvidenceFile::decode(&bytes).unwrap();
            let rebuilt = file.lettering(group).unwrap();
            assert_eq!(sha256_hex(&buffers::encode_mask(&rebuilt)), group.lettering_sha256);
            assert!(row.id.contains("-d"), "region ids keep their format: {}", row.id);
        }
    }

    /// The auto one-pass path writes the same evidence file Detect does, before
    /// the patch row whose provenance names it.
    #[test]
    fn a_one_pass_run_writes_the_evidence_its_patches_name() {
        let _exclusive = exclusively();
        let scratch = Scratch::new("grouped-one-pass");
        let library = Library::at(scratch.join("library"));
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        std::fs::write(raws.join("001.png"), encode(&grouped_page(), Format::Png).unwrap()).unwrap();
        let project = library.create_project("Groups", StripMode::Single, Some(raws), None).unwrap();
        let chapter = library.create_chapter(&project.id, "Ch. 1", None, None).unwrap().created().unwrap();
        let entries = plan_for(&library, "chapter", &chapter.id, None, RunMode::Auto).unwrap();
        let recorder = Recorder::new();
        let mut pipeline = pipeline(&scratch);
        let summary = execute_as(RunMode::Auto, "run-1", &chapter.id, &entries, &mut pipeline,
            Engine::Fill, &recorder.cancel, &recorder.sink());
        assert_eq!((summary.regions_cleaned, summary.errored), (3, 0));
        let job = Job::open(&entries[0].job_path).unwrap();
        assert_eq!(job.project.patches.len(), 3);
        // The unboxed lettering is a patch too, and not a review flag.
        assert!(job.project.regions_untouched.is_empty());
        assert!(job.project.patches.iter().all(|patch| patch.review_state.is_none()));
        for patch in &job.project.patches {
            let group = &patch.provenance.params_snapshot["group"];
            let reference = group["evidence"].as_str().expect("a grouped patch names its evidence");
            let bytes = std::fs::read(job.sidecar().join(reference)).expect("the named evidence file exists");
            assert_eq!(reference, format!("evidence/{}.mtev", sha256_hex(&bytes)));
            let file = EvidenceFile::decode(&bytes).unwrap();
            let id = group["id"].as_str().unwrap();
            let held = file.grouping.groups.iter().find(|g| g.id == id).expect("the group is in its evidence");
            let lettering = file.grouping.lettering(held);
            let (dx, dy) = file.grouping.origin;
            let on_page = cleaner_core::mask::Mask {
                bounds: Rect::new(lettering.bounds.x + dx, lettering.bounds.y + dy, lettering.bounds.w, lettering.bounds.h),
                bits: lettering.bits,
            };
            assert_eq!(Some(sha256_hex(&buffers::encode_mask(&on_page)).as_str()), group["letteringSha256"].as_str());
        }
    }

    /// Two touching free captions an Ogkalu-only run finds, and a box over
    /// flat paper.
    fn caption_balloons(_: &Raster, _: usize) -> Vec<BalloonBox> {
        vec![
            BalloonBox { rect: Rect::new(90, 30, 90, 30), class: BalloonClass::TextFree, score: 0.9 },
            BalloonBox { rect: Rect::new(90, 58, 90, 30), class: BalloonClass::TextFree, score: 0.9 },
            BalloonBox { rect: Rect::new(250, 250, 60, 30), class: BalloonClass::TextFree, score: 0.9 },
        ]
    }

    fn caption_page() -> Raster {
        let mut page = gray_test_page(400, 400);
        for x in [100, 124, 148] {
            glyph(&mut page, x, 36, 18);
            glyph(&mut page, x, 66, 18);
        }
        // A stroke through both boxes' tight tiers.
        paint(&mut page, Rect::new(100, 57, 60, 3));
        page
    }

    /// Without a pixel model the text boxes are still text groups: each seeds
    /// from a bounded ink estimate under it, never the box, and two touching
    /// captions never share a seed pixel.
    #[test]
    fn an_ogkalu_only_run_seeds_exclusive_ink_estimates_not_rectangles() {
        let scratch = Scratch::new("ogkalu-only");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect, balloons: caption_balloons, judge });
        pipeline.test_lettering = None;
        pipeline.detection_models = DetectorModels { ctd: false, rt_small: true, rt_full: false, sam: false };
        let page = caption_page();
        let SegmentDetection { detection, grouping, .. } = pipeline.detect_segment(&page, 0, (0, 0), "").unwrap();
        assert!(detection.segmentation.levels.iter().all(|v| *v == 0), "no box was painted in");
        assert_eq!(grouping.cleaning().count(), 0);
        let groups: Vec<_> = grouping.detector_only().collect();
        assert_eq!(groups.len(), 3);
        let mut claimed = std::collections::HashSet::new();
        for group in &groups {
            let lettering = grouping.lettering(group);
            let anchor = group.anchor.unwrap();
            if anchor.x == 250 {
                assert!(lettering.is_empty() && !group.estimated, "flat paper has no ink");
                continue;
            }
            assert!(group.estimated);
            assert!(lettering.count() * 100 < (anchor.w * anchor.h) as usize * 45, "{}", group.id);
            for y in lettering.bounds.y..lettering.bounds.bottom() {
                for x in lettering.bounds.x..lettering.bounds.right() {
                    if lettering.contains(x, y) {
                        assert_eq!(page.data[(y * 400 + x) as usize], 30, "an estimate took paper");
                        assert!(claimed.insert((x, y)), "({x}, {y}) seeds two groups");
                    }
                }
            }
        }
        assert!(claimed.contains(&(120, 58)), "the shared stroke is seeded once");

        // Detect stores them as groups with their estimate in the evidence;
        // the flat box is listed with its reason.
        let bytes = encode(&page, Format::Png).unwrap();
        let strip = Strip::of_sizes(&[(page.width, page.height)]);
        let survey = cleaner_core::strip::survey(&strip, |_| None);
        let context = single_page_context(&strip, &survey);
        let outcome = pipeline.detect_page("c1-p001", &bytes, Engine::Fill, &context).unwrap();
        let detected: Vec<&LoadedDetection> = outcome.regions.iter().filter_map(|region| match region {
            RegionOutcome::Detected(found) => Some(found.as_ref()),
            _ => None,
        }).collect();
        assert_eq!(detected.len(), 2, "{:?}", outcome.regions.iter().map(one_line).collect::<Vec<_>>());
        for found in detected {
            let group = found.record.group.as_ref().unwrap();
            assert!(group.estimated && group.components.is_empty());
            let file = EvidenceFile::decode(&found.evidence.as_ref().unwrap().bytes).unwrap();
            let rebuilt = file.lettering(group).unwrap();
            assert_eq!(sha256_hex(&buffers::encode_mask(&rebuilt)), group.lettering_sha256);
            assert!(rebuilt.count() < (group.bounds.w * group.bounds.h) as usize);
        }
        assert!(outcome.regions.iter().any(|region| matches!(region, RegionOutcome::Untouched { bbox, .. }
            if bbox.contains(260, 260))));
    }

    /// CTD-only: CTD's segmentation is the pixel evidence, grouped by CTD's
    /// boxes, and never replaced by a rectangle.
    #[test]
    fn a_ctd_only_run_groups_ctd_pixels() {
        fn ctd_detect(crop: &Raster, _: usize) -> Detection {
            let levels = (0..crop.height).flat_map(|y| (0..crop.width).map(move |x| (x, y)))
                .map(|(x, y)| if y < 100 && crop.luma16_at(x, y) >> 8 < 128 { 200 } else { 10 })
                .collect();
            Detection {
                boxes: vec![DetBox { rect: Rect::new(92, 32, 80, 60), confidence: 0.9,
                    language: cleaner_core::detect::DetectedLanguage::Japanese }],
                segmentation: Segmentation { width: crop.width, height: crop.height, levels,
                    fit: Letterbox::fit(crop.width, crop.height) },
            }
        }
        let scratch = Scratch::new("ctd-only");
        let mut pipeline = pipeline(&scratch);
        pipeline.test_vision = Some(TestVision { detect: ctd_detect, balloons: caption_balloons, judge });
        pipeline.test_lettering = None;
        pipeline.detection_models = DetectorModels { ctd: true, rt_small: false, rt_full: false, sam: false };
        let page = caption_page();
        let SegmentDetection { balloons, grouping, .. } = pipeline.detect_segment(&page, 0, (0, 0), "").unwrap();
        assert!(balloons.is_empty(), "no bubble detector ran");
        assert_eq!(grouping.pixel_model, cleaner_core::text_groups::EvidenceModel::Ctd);
        let groups: Vec<_> = grouping.cleaning().collect();
        assert_eq!(groups.len(), 1);
        assert!(groups[0].component_ids.iter().all(|id| id.starts_with("seg-")) && !groups[0].estimated);
        let lettering = grouping.lettering(groups[0]);
        let dark = (0..100).flat_map(|y| (0..400).map(move |x| (x, y)))
            .filter(|&(x, y)| page.data[y * 400 + x] == 30).count();
        assert_eq!(lettering.count(), dark, "exactly CTD's pixels above its threshold");
    }
}


/// Exercise the cloud upload and the real detector/manifest path together.
fn repeat_cloud_detect(scratch: &Scratch, library: &Library, chapter_id: &str, prefetch: bool)
    -> (Summary, Vec<Raster>) {
    use crate::inference::run_analysis::{fake, RemoteDetection};
    use cleaner_core::cloud_analysis_wire::SAM;
    let entries = plan_cloud_detection(library, "chapter", chapter_id, None).unwrap();
    let gateway = fake::gateway();
    let remote = RemoteDetection::new(Box::new(gateway.clone()),
        fake::scope("modal-repeat-detection", &[0], &[SAM]));
    let mut detector = lettered_pipeline(scratch).with_remote(Some(remote));
    detector.detection_models.sam = true;
    if prefetch { detector.prefetch_remote(&entries); }
    let recorder = Recorder::new();
    let summary = execute_as(RunMode::Detect, "repeat-cloud", chapter_id, &entries,
        &mut detector, Engine::Lama, &recorder.cancel, &recorder.sink());
    drop(detector);
    let uploads = gateway.uploads.lock().unwrap().iter().map(|bytes| decode(bytes).unwrap()).collect();
    (summary, uploads)
}

fn repeat_detection_patch(job: &mut Job, page_id: &str, value: u8, visible: bool, empty: bool) {
    let id = job.next_detection_id(page_id);
    let mut patch = a_patch(&id, job.project.patches.len() as u32, Engine::Fill);
    let bounds = Rect::new(40, 55, 28, 28);
    patch.mask = Mask::filled(bounds);
    // Keep text elsewhere inside this box, so Detect must accept a new
    // region overlapping the old record's bbox, even with a visible edit.
    patch.mask.bits.fill(0);
    if !empty { patch.mask.bits[4 * 28 + 4] = 255; }
    patch.ink = patch.mask.clone();
    patch.pixels.width = bounds.w;
    patch.pixels.height = bounds.h;
    patch.pixels.data = vec![value; (bounds.w * bounds.h) as usize];
    patch.visible = visible;
    job.complete_region(0, &patch, None).unwrap();
}

#[test]
fn repeat_cloud_detection_on_a_cleaned_page_uploads_visible_edits() {
    let _exclusive = exclusively();
    let scratch = Scratch::new("repeat-cloud-cleaned");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 1);
    let page_id = library::page_id(&chapter_id, 0);
    let mut job = only_job(&library, &chapter_id);
    repeat_detection_patch(&mut job, &page_id, 77, true, false);
    repeat_detection_patch(&mut job, &page_id, 88, false, false);
    repeat_detection_patch(&mut job, &page_id, 99, true, true);
    job.mark_examined(0).unwrap();
    job.project.detected.push(0);
    job.flush().unwrap();
    let before = job.project.patches.clone();
    assert!(plan(&library, "chapter", &chapter_id, None).unwrap().is_empty(), "local Auto keeps its queue");
    let (summary, uploads) = repeat_cloud_detect(&scratch, &library, &chapter_id, true);
    assert_eq!((summary.pages_queued, summary.errored), (1, 0));
    assert_eq!(summary.regions_detected, 2, "old patch boxes cannot suppress this pass");
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].rgb8_pixel(44, 59), [77; 3], "visible edit is uploaded; hidden/deleted layers are excluded");
    let after = only_job(&library, &chapter_id);
    assert_eq!(after.project.patches, before);
    assert!(after.project.detections.iter().all(|row| !before.iter().any(|old| old.id == row.id)));
}

#[test]
fn repeat_cloud_detection_after_deleting_all_masks_uploads_the_original() {
    let _exclusive = exclusively();
    let scratch = Scratch::new("repeat-cloud-deleted");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 1);
    let page_id = library::page_id(&chapter_id, 0);
    let mut job = only_job(&library, &chapter_id);
    repeat_detection_patch(&mut job, &page_id, 77, true, false);
    job.mark_examined(0).unwrap();
    // Use the same soft-delete operation as Delete mask and retain its row.
    let id = job.project.patches[0].id.clone();
    library.set_mask_visible(&id, false).unwrap().unwrap();
    let job = only_job(&library, &chapter_id);
    let before = job.project.patches.clone();
    let source = decode(&std::fs::read(job.source_path(0).unwrap()).unwrap()).unwrap();
    // Cover the non-prefetched cloud path too.
    let (summary, uploads) = repeat_cloud_detect(&scratch, &library, &chapter_id, false);
    assert_eq!((summary.regions_detected, summary.errored), (2, 0));
    assert_eq!(uploads.len(), 1);
    for y in 0..source.height {
        for x in 0..source.width {
            assert_eq!(uploads[0].rgb8_pixel(x, y), source.rgb8_pixel(x, y));
        }
    }
    let after = only_job(&library, &chapter_id);
    assert_eq!(after.project.patches, before, "deleted record remains available for undo");
    assert!(after.project.detections.iter().all(|row| row.id != before[0].id));
}

#[test]
fn repeat_cloud_detection_preserves_existing_masks_edits_and_undo_history() {
    use crate::history::{Journal, NewDelta, Side};
    let _exclusive = exclusively();
    let scratch = Scratch::new("repeat-cloud-history");
    let (library, chapter_id) = a_lettered_chapter(&scratch, 1);
    let (first, _) = repeat_cloud_detect(&scratch, &library, &chapter_id, true);
    assert_eq!(first.regions_detected, 2);
    let mut job = only_job(&library, &chapter_id);
    // A manually edited detection must survive a later request intact.
    let mut edited = job.load_detection(&job.project.detections[0].id).unwrap().unwrap();
    edited.record.pick = "solid".into();
    edited.record.balloon_color = Some([123; 3]);
    let at = edited.mask.bits.iter().position(|pixel| *pixel > 0).unwrap();
    edited.mask.bits[at] = 0;
    edited.ink.bits[at] = 0;
    job.rewrite_page_detections(0, &library::page_id(&chapter_id, 0), vec![edited], Vec::new(), &[]).unwrap();
    repeat_detection_patch(&mut job, &library::page_id(&chapter_id, 0), 77, true, false);
    let detections = job.project.detections.clone();
    let patches = job.project.patches.clone();
    let masks: Vec<_> = detections.iter().map(|row| job.load_detection(&row.id).unwrap().unwrap().mask).collect();
    let path = library.resolve_chapter(&chapter_id).unwrap();
    let mut journal = Journal::default();
    journal.push(NewDelta {
        label: "masks.command.rerunMask".into(), op: "region-state".into(), region_id: patches[0].id.clone(),
        before: Side { present: true, page_status: Some("cleaned".into()), region: None },
        after: Side { present: true, page_status: Some("cleaned".into()), region: None },
    });
    journal.undo(); // Preserve the redo stack as well as existing entries.
    crate::history::save(&path, &journal).unwrap();
    let history_before = std::fs::read(crate::history::journal_path(&path)).unwrap();
    let (second, _) = repeat_cloud_detect(&scratch, &library, &chapter_id, true);
    assert_eq!((second.regions_detected, second.errored), (2, 0));
    let after = only_job(&library, &chapter_id);
    assert_eq!(after.project.patches, patches);
    let saved_patch = after.load_patch(&patches[0]).unwrap();
    assert_eq!(saved_patch.pixels.data, job.load_patch(&patches[0]).unwrap().pixels.data);
    assert_eq!(saved_patch.mask, job.load_patch(&patches[0]).unwrap().mask);
    assert_eq!(&after.project.detections[..detections.len()], &detections);
    for (row, mask) in detections.iter().zip(masks) {
        assert_eq!(after.load_detection(&row.id).unwrap().unwrap().mask, mask);
    }
    let ids: std::collections::HashSet<_> = after.project.detections.iter().map(|row| &row.id).collect();
    assert_eq!(ids.len(), after.project.detections.len());
    assert_eq!(std::fs::read(crate::history::journal_path(&path)).unwrap(), history_before);
}

#[test]
fn native_fill_refuses_associated_alpha_and_unsupported_tiff_before_rendering() {
    for associated in [true,false] {
        let mut page=fixtures::by_name("rgba8").raster;
        let fitted=fitted_over(&page,Rect::new(8,8,12,10));
        if associated {page.color.associated_alpha=true;} else {page.color.unsupported_tiff_color=vec![301];}
        let error=match clean_region_with_color(&mut no_rung_two(),&page,&fitted,Engine::Fill,Some(EnginePick::Fill),0.0,None) {
            Err(error)=>error,Ok(_)=>panic!("unsafe source-derived fill reached rendering")
        };
        assert!(error.contains(if associated {"associated-alpha"} else {"TIFF"}),"{error}");
    }
}
