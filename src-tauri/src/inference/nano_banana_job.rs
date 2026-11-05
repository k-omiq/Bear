//! A Nano Banana batch: the stored detections of a page or a chapter, cleaned
//! one at a time with Gemini, as a job of its own.
//!
//! It holds a run slot like a cloud clean does (`run::start_reserved_job`), so
//! it goes on when the editor leaves the chapter, `list_jobs` lists it, the
//! quit guard counts it, Stop reaches it from the jobs list as well as from
//! the editor (`cancel_run`, which also ends the request in flight and
//! withdraws the grant), and it holds each page it is on against other
//! writers. Each region is cleaned through the same path a click takes
//! (`region::clean_for_nano_banana_batch`), on the run's behalf, so the page
//! hold does not refuse it and its undo steps join one step for the batch.
//!
//! ## Consent
//!
//! The person confirmed a count and a cost in the window
//! (`nanobananaask.svelte.js`), which made a grant for that many regions
//! (`nano_banana_consent.rs`). The batch is refused before it starts unless
//! that grant is live and covers every region it was handed, and each region
//! spends one share of it just before its first request. A retry of the same
//! region uses the same share, so the count is never more than the window
//! said, and it is asked again for before each retry: Stop or Cloud engines
//! turned off withdraws it, and the retry does not go.
//!
//! ## What a failure means
//!
//! [`Failure`] says what Google answered, and that decides what the batch does:
//!
//! - **The account** ([`Verdict::Account`]): no key, a key store that will not
//!   answer, a rejected or expired key, a country Google does not serve,
//!   billing, a refused model, Cloud engines off. Every other region would
//!   fail the same way, so the batch stops at once and says why.
//! - **Busy** ([`Verdict::Busy`]): a rate limit, or Google not reachable. The
//!   region is tried again after a wait ([`RETRY_WAITS`]), and when the waits
//!   are used up the batch stops, because the next region would wait too.
//! - **The crop** ([`Verdict::Crop`]): a safety refusal, a recitation, an
//!   answer that could not be used or kept the mark, a page changed while the
//!   request was out, a region too large. Another region may well go through,
//!   so this one is skipped with its reason and the batch goes on.
//! - **Stopped** ([`Verdict::Stopped`]): Stop, or the grant withdrawn, lapsed
//!   or replaced by another Nano Banana window. The batch ends as cancelled.
//!
//! At the end the batch sends its [`Report`] (`nano-banana-report`) and one
//! notice. The interface shows the counts per reason from the report, and can
//! start a new batch for the regions it did not clean, behind a new window.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

use super::nano_banana::{Config, Failure};
use super::nano_banana_consent::{Book, CLOUD_OFF, MAX_REGIONS, NO_CONSENT};
use crate::events::{self, Event};
use crate::library::{self, ApiRegion, Library};
use crate::run::{self, QueuedPage, RunHandle, Summary};

/// The waits before each retry of a region that met a rate limit or could not
/// reach Google. The request itself already waited once, up to a minute, for a
/// server that named a wait (`nano_banana.rs#refused`). Two more tries, then
/// the batch stops.
pub const RETRY_WAITS: [Duration; 2] = [Duration::from_secs(15), Duration::from_secs(45)];

/// The reason a region gets when its clean failed for a cause no reason key
/// names (a manifest that could not be written, another process holding the
/// chapter).
pub const ERROR_REASON: &str = "nanoBanana.job.reason.error";

/// What a reason means for the batch. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Stopped,
    Account,
    Busy,
    Crop,
}

/// What `reason` means for the batch.
pub(crate) fn verdict(reason: &str) -> Verdict {
    let is = |failures: &[Failure]| failures.iter().any(|failure| failure.reason_key() == reason);
    if reason == NO_CONSENT || is(&[Failure::Stopped]) {
        Verdict::Stopped
    } else if reason == CLOUD_OFF
        || is(&[Failure::NoKey, Failure::KeyStore, Failure::KeyRejected, Failure::KeyExpired,
            Failure::Location, Failure::Billing, Failure::ModelRejected])
    {
        Verdict::Account
    } else if is(&[Failure::RateLimited, Failure::Unreachable]) {
        Verdict::Busy
    } else {
        Verdict::Crop
    }
}

/// One region the batch was handed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub page_index: u32,
    pub region_id: String,
}

/// A region the batch did not clean, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unclean {
    pub region_id: String,
    pub page_index: u32,
    /// A `decline.reason.*` key, or [`ERROR_REASON`].
    pub reason: String,
}

/// How a batch ended, as `nano-banana-report` carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Regions the batch was handed.
    pub total: u32,
    pub cleaned: u32,
    /// Regions no longer detections when their turn came: nothing was sent.
    pub gone: u32,
    /// Regions tried and not cleaned, each with its reason.
    pub failed: Vec<Unclean>,
    /// Regions the batch did not reach, because it stopped first.
    pub left: Vec<Target>,
    /// Why the batch stopped early: the reason key, or none when it went to
    /// the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// Whether the stop was the person's Stop. A grant used up, lapsed or
    /// replaced by another Nano Banana window also ends the batch, as
    /// cancelled, and says so.
    pub by_user: bool,
}

/// One region's clean, as the batch sees it.
pub(crate) enum Attempt {
    Cleaned(Option<Box<ApiRegion>>),
    Gone,
    Refused(&'static str),
    Error(String),
}

/// Everything the batch's walk needs that is not the walk: a seam, so the
/// order of events, the grant and the failure handling are tested without a
/// window or a request.
pub(crate) struct Walk<'a> {
    pub run_id: &'a str,
    pub chapter_id: &'a str,
    pub grants: &'a Book,
    pub nonce: &'a str,
    pub cloud_allowed: &'a dyn Fn() -> bool,
    /// Clean one region under the confirmed settings.
    pub clean: &'a dyn Fn(&Target, &Config) -> Attempt,
    /// The page's `page-done`, as the manifest now holds it.
    pub page_done: &'a dyn Fn(u32) -> Event,
    /// Wait this long, or less when `cancel` goes up. Answers whether the
    /// whole wait passed.
    pub sleep: &'a dyn Fn(Duration, &AtomicBool) -> bool,
    pub waits: &'a [Duration],
}

/// Walk `targets` page by page. Answers the report and the run's summary.
pub(crate) fn walk(walk: &Walk<'_>, targets: &[Target], cancel: &AtomicBool, emit: &dyn Fn(Event))
    -> (Report, Summary) {
    let mut pages: BTreeMap<u32, Vec<&Target>> = BTreeMap::new();
    for target in targets {
        pages.entry(target.page_index).or_default().push(target);
    }
    let mut report = Report { total: u32::try_from(targets.len()).unwrap_or(u32::MAX), ..Report::default() };
    let mut pages_done = 0u32;
    let mut stopped: Option<&'static str> = None;
    for (page, on_page) in &pages {
        if stopped.is_none() && cancel.load(Ordering::SeqCst) {
            stopped = Some(Failure::Stopped.reason_key());
        }
        if stopped.is_some() {
            report.left.extend(on_page.iter().map(|target| (*target).clone()));
            continue;
        }
        emit(run::page_started_event(walk.run_id, walk.chapter_id, *page));
        for target in on_page {
            if stopped.is_none() && cancel.load(Ordering::SeqCst) {
                stopped = Some(Failure::Stopped.reason_key());
            }
            if stopped.is_some() {
                report.left.push((*target).clone());
                continue;
            }
            match clean_one(walk, target, cancel) {
                Attempt::Cleaned(region) => {
                    report.cleaned += 1;
                    if let Some(region) = region {
                        emit(Event::RegionDone {
                            run_id: walk.run_id.to_owned(),
                            chapter_id: walk.chapter_id.to_owned(),
                            page_id: library::page_id(walk.chapter_id, target.page_index as usize),
                            page_index: target.page_index,
                            region,
                        });
                    }
                }
                Attempt::Gone => report.gone += 1,
                Attempt::Error(error) => {
                    eprintln!("manga-cleaner: a Nano Banana batch could not clean {}: {error}", target.region_id);
                    report.failed.push(unclean(target, ERROR_REASON));
                }
                Attempt::Refused(reason) => match verdict(reason) {
                    Verdict::Crop => report.failed.push(unclean(target, reason)),
                    Verdict::Stopped | Verdict::Account | Verdict::Busy => {
                        stopped = Some(reason);
                        report.left.push((*target).clone());
                    }
                },
            }
        }
        emit((walk.page_done)(*page));
        pages_done += 1;
    }
    report.by_user = stopped == Some(Failure::Stopped.reason_key());
    report.stopped = stopped.map(str::to_owned);
    let reason = match stopped.map(verdict) {
        None => "completed",
        Some(Verdict::Stopped) => "cancelled",
        Some(_) => "failed",
    };
    let summary = Summary {
        reason,
        pages_queued: u32::try_from(pages.len()).unwrap_or(u32::MAX),
        pages_cleaned: pages_done,
        regions_cleaned: report.cleaned,
        next_page_index: None,
        errored: 0,
        regions_detected: 0,
        regions_declined: u32::try_from(report.failed.len()).unwrap_or(u32::MAX),
    };
    (report, summary)
}

fn unclean(target: &Target, reason: &str) -> Unclean {
    Unclean { region_id: target.region_id.clone(), page_index: target.page_index, reason: reason.to_owned() }
}

/// One region: its share of the grant, then the clean, tried again after each
/// wait while Google is busy, as long as the grant still holds.
fn clean_one(walk: &Walk<'_>, target: &Target, cancel: &AtomicBool) -> Attempt {
    let spend = match walk.grants.spend((walk.cloud_allowed)(), Some(walk.nonce)) {
        Ok(spend) => spend,
        Err(reason) => return Attempt::Refused(reason),
    };
    let mut waits = walk.waits.iter();
    loop {
        let attempt = run::on_behalf_of(walk.run_id, || (walk.clean)(target, &spend.config));
        let Attempt::Refused(reason) = attempt else { return attempt };
        if verdict(reason) != Verdict::Busy {
            return attempt;
        }
        let Some(wait) = waits.next() else { return attempt };
        eprintln!("manga-cleaner: Nano Banana is busy ({reason}), trying {} again in {}s", target.region_id, wait.as_secs());
        if !(walk.sleep)(*wait, cancel) {
            return Attempt::Refused(Failure::Stopped.reason_key());
        }
        if let Err(withdrawn) = walk.grants.left((walk.cloud_allowed)(), walk.nonce) {
            return Attempt::Refused(withdrawn);
        }
    }
}

/// The notice a batch ends on: its counts, and why it stopped when it did.
fn closing_notice(run_id: &str, report: &Report) -> Event {
    let counts = |reason: Option<&str>| {
        let mut params = serde_json::json!({
            "runId": run_id,
            "cleaned": report.cleaned,
            "total": report.total,
            "failed": report.failed.len(),
        });
        if let Some(reason) = reason {
            params["reasonKey"] = serde_json::json!(reason);
        }
        params
    };
    match report.stopped.as_deref() {
        Some(_) if report.by_user => events::notice_event("nanoBanana.job.stopped", counts(None), "warn"),
        Some(reason) => events::notice_event("nanoBanana.job.halted", counts(Some(reason)), "warn"),
        None if report.failed.is_empty() => events::notice_event("nanoBanana.job.finished", counts(None), "info"),
        None => events::notice_event("nanoBanana.job.finishedWithFailures", counts(None), "warn"),
    }
}

/// Wait `wait` in short steps, and stop waiting when `cancel` goes up.
fn sleep_unless_cancelled(wait: Duration, cancel: &AtomicBool) -> bool {
    let tick = Duration::from_millis(100);
    let mut left = wait;
    while !left.is_zero() {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        let step = left.min(tick);
        std::thread::sleep(step);
        left -= step;
    }
    !cancel.load(Ordering::SeqCst)
}

/// The batch's regions, checked: each named once, all of `chapter_id`, and no
/// more than a grant can cover. Sorted by page, in the order given within one.
fn targets_of(chapter_id: &str, region_ids: &[String]) -> Result<Vec<Target>, String> {
    if region_ids.is_empty() {
        return Err("nano_banana_no_regions".into());
    }
    if region_ids.len() > MAX_REGIONS as usize {
        return Err(NO_CONSENT.into());
    }
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for region_id in region_ids {
        if !seen.insert(region_id.as_str()) {
            continue;
        }
        let page_index = crate::region::page_index_in(chapter_id, region_id)
            .and_then(|page| u32::try_from(page).ok())
            .ok_or_else(|| format!("nano_banana_region_not_in_chapter: {region_id}"))?;
        targets.push(Target { page_index, region_id: region_id.clone() });
    }
    targets.sort_by_key(|target| target.page_index);
    Ok(targets)
}

/// Start the batch, or answer with the run already holding the chapter.
/// Refused before anything is reserved while Cloud engines are off and when
/// the grant `nano_banana_grant` is not live or covers fewer regions than it
/// was handed.
fn start(app: &tauri::AppHandle, chapter_id: &str, region_ids: &[String], nano_banana_grant: Option<&str>)
    -> Result<RunHandle, String> {
    let grants = super::nano_banana_consent::book();
    let nonce = nano_banana_grant.ok_or(NO_CONSENT)?.to_owned();
    let targets = targets_of(chapter_id, region_ids)?;
    let left = grants.left(super::cloud_allowed(app), &nonce)?;
    if (left as usize) < targets.len() {
        return Err(NO_CONSENT.into());
    }
    let reservation = match run::reserve_run(chapter_id, &[], run::RunKind::NanoBanana)? {
        Ok(reservation) => reservation,
        Err(busy) => return Ok(busy),
    };
    let library = Library::for_app(app).map_err(|e| e.to_string())?;
    let job_path = library.resolve_chapter(chapter_id).map_err(|e| e.to_string())?;
    reservation.set_jobs([&job_path]);
    let pages: Vec<QueuedPage> = targets.iter().map(|target| target.page_index).collect::<BTreeSet<_>>()
        .into_iter()
        .map(|page_index| QueuedPage {
            chapter_id: chapter_id.to_owned(),
            page_id: library::page_id(chapter_id, page_index as usize),
            page_index,
        })
        .collect();
    let app = app.clone();
    let chapter = chapter_id.to_owned();
    Ok(run::start_reserved_job(reservation, chapter_id, pages, move |run_id, cancel, emit| {
        let clean = |target: &Target, config: &Config| {
            match crate::region::clean_for_nano_banana_batch(&app, &library, &target.region_id, config) {
                Ok(crate::region::BatchRegion::Cleaned(region)) => Attempt::Cleaned(Some(region)),
                Ok(crate::region::BatchRegion::Gone) => Attempt::Gone,
                Ok(crate::region::BatchRegion::Refused(reason)) => Attempt::Refused(reason),
                Err(error) => Attempt::Error(error),
            }
        };
        let page_done = |page: u32| run::page_done_after_clean(run_id, &chapter, &job_path, page);
        let cloud_allowed = || super::cloud_allowed(&app);
        let walk_with = Walk {
            run_id, chapter_id: &chapter, grants, nonce: &nonce, cloud_allowed: &cloud_allowed,
            clean: &clean, page_done: &page_done, sleep: &sleep_unless_cancelled, waits: &RETRY_WAITS,
        };
        let (report, summary) = walk(&walk_with, &targets, cancel, emit);
        finish(run_id, &chapter, &report, emit);
        summary
    }, run::Closing::Body))
}

/// The report, then the closing notice, before the run's `run-finished`.
fn finish(run_id: &str, chapter_id: &str, report: &Report, emit: &dyn Fn(Event)) {
    emit(Event::NanoBananaReport { run_id: run_id.to_owned(), chapter_id: chapter_id.to_owned(), report: report.clone() });
    emit(closing_notice(run_id, report));
}

/// `startNanoBananaRun`: clean `region_ids`, stored detections of
/// `chapter_id`, with Nano Banana, on the grant the window just made. Answers
/// the run's handle as `runClean` does, `alreadyRunning` included.
#[tauri::command]
pub async fn start_nano_banana_run(
    app: tauri::AppHandle,
    chapter_id: String,
    region_ids: Vec<String>,
    nano_banana_grant: Option<String>,
) -> Result<RunHandle, String> {
    tauri::async_runtime::spawn_blocking(move || start(&app, &chapter_id, &region_ids, nano_banana_grant.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::nano_banana_consent::IDLE;
    use cleaner_core::engines::nano_banana::Model;
    use std::cell::RefCell;
    use std::sync::Mutex;

    fn config() -> Config {
        Config { model: Model::DEFAULT, instruction: None }
    }

    fn target(page: u32, id: &str) -> Target {
        Target { page_index: page, region_id: id.to_owned() }
    }

    fn page_done(page: u32) -> Event {
        Event::PageDone {
            run_id: "run-nb".into(),
            chapter_id: "c1".into(),
            page_id: library::page_id("c1", page as usize),
            page_index: page,
            page: Box::new(crate::library::ApiPage {
                id: library::page_id("c1", page as usize), chapter_id: "c1".into(), index: page, source_index: 0,
                number: page + 1, file: String::new(), source_sha: String::new(), width: 0, height: 0,
                status: "unclean", skip_reason: None, regions: Vec::new(), region_count: 0, done_count: 0,
                review_count: 0, candidate_count: 0, resident: true, appearance: String::new(),
            }),
        }
    }

    /// The walk over `targets`, each region answered by `answer` in turn, with
    /// a grant for `granted` regions. Answers the report, the summary, the
    /// event types in order and the waits asked for.
    fn run_walk(
        targets: &[Target],
        granted: u32,
        answer: impl Fn(&str, usize) -> Attempt,
        cancel_after: Option<usize>,
    ) -> (Report, Summary, Vec<String>, Vec<Duration>, Result<u32, &'static str>) {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(), granted).unwrap();
        let tries = Mutex::new(Vec::<String>::new());
        let cancel = AtomicBool::new(false);
        let clean = |target: &Target, _: &Config| {
            let mut seen = tries.lock().unwrap();
            let attempt = seen.iter().filter(|id| **id == target.region_id).count();
            seen.push(target.region_id.clone());
            if cancel_after.is_some_and(|after| seen.len() >= after) {
                cancel.store(true, Ordering::SeqCst);
            }
            answer(&target.region_id, attempt)
        };
        let waited = Mutex::new(Vec::new());
        let sleep = |wait: Duration, cancel: &AtomicBool| {
            waited.lock().unwrap().push(wait);
            !cancel.load(Ordering::SeqCst)
        };
        let events = RefCell::new(Vec::new());
        let emit = |event: Event| events.borrow_mut().push(serde_json::to_value(&event).unwrap()["type"].as_str().unwrap().to_owned());
        let allowed = || true;
        let walk_with = Walk {
            run_id: "run-nb", chapter_id: "c1", grants: &book, nonce: &nonce, cloud_allowed: &allowed,
            clean: &clean, page_done: &page_done, sleep: &sleep, waits: &RETRY_WAITS,
        };
        let (report, summary) = walk(&walk_with, targets, &cancel, &emit);
        let types = events.into_inner();
        let waits = waited.into_inner().unwrap();
        let left = book.left(true, &nonce);
        (report, summary, types, waits, left)
    }

    fn cleaned(_: &str, _: usize) -> Attempt {
        Attempt::Cleaned(None)
    }

    #[test]
    fn every_failure_has_its_verdict() {
        for failure in [Failure::NoKey, Failure::KeyStore, Failure::KeyRejected, Failure::KeyExpired,
            Failure::Location, Failure::Billing, Failure::ModelRejected] {
            assert_eq!(verdict(failure.reason_key()), Verdict::Account, "{failure:?}");
        }
        assert_eq!(verdict(CLOUD_OFF), Verdict::Account);
        for failure in [Failure::RateLimited, Failure::Unreachable] {
            assert_eq!(verdict(failure.reason_key()), Verdict::Busy, "{failure:?}");
        }
        for failure in [Failure::Blocked, Failure::Recited, Failure::BadAnswer, Failure::KeptMark] {
            assert_eq!(verdict(failure.reason_key()), Verdict::Crop, "{failure:?}");
        }
        assert_eq!(verdict("decline.reason.nanoBananaPageChanged"), Verdict::Crop);
        assert_eq!(verdict(ERROR_REASON), Verdict::Crop);
        assert_eq!(verdict(Failure::Stopped.reason_key()), Verdict::Stopped);
        assert_eq!(verdict(NO_CONSENT), Verdict::Stopped);
    }

    #[test]
    fn a_refused_crop_is_skipped_with_its_reason_and_the_batch_goes_on() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(1, "c1-p002-d0"), target(1, "c1-p002-d1")];
        let (report, summary, types, waits, _) = run_walk(&targets, 4, |id, _| match id {
            "c1-p001-d0" => Attempt::Refused(Failure::Blocked.reason_key()),
            "c1-p002-d0" => Attempt::Refused(Failure::KeptMark.reason_key()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!(report.cleaned, 2);
        assert_eq!(report.failed, vec![
            unclean(&targets[0], Failure::Blocked.reason_key()),
            unclean(&targets[2], Failure::KeptMark.reason_key()),
        ]);
        assert!(report.left.is_empty() && report.stopped.is_none());
        assert_eq!(summary.reason, "completed");
        assert_eq!((summary.pages_queued, summary.pages_cleaned, summary.regions_cleaned), (2, 2, 2));
        assert_eq!(types, ["page-started", "page-done", "page-started", "page-done"]);
        assert!(waits.is_empty());
    }

    #[test]
    fn an_account_failure_stops_the_batch_at_once() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(2, "c1-p003-d0")];
        let (report, summary, types, _, _) = run_walk(&targets, 3, |id, _| match id {
            "c1-p001-d1" => Attempt::Refused(Failure::KeyRejected.reason_key()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!(report.cleaned, 1);
        assert!(report.failed.is_empty());
        assert_eq!(report.left, vec![targets[1].clone(), targets[2].clone()]);
        assert_eq!(report.stopped.as_deref(), Some(Failure::KeyRejected.reason_key()));
        assert!(!report.by_user);
        assert_eq!(summary.reason, "failed");
        // The page it was on is finished; the next is never started.
        assert_eq!(types, ["page-started", "page-done"]);
        let notice = closing_notice("run-nb", &report);
        let Event::Notice { key, params, .. } = notice else { panic!("not a notice") };
        assert_eq!(key, "nanoBanana.job.halted");
        assert_eq!(params["reasonKey"], Failure::KeyRejected.reason_key());
        assert_eq!((params["cleaned"].as_u64(), params["total"].as_u64()), (Some(1), Some(3)));
    }

    #[test]
    fn a_busy_server_is_waited_for_and_tried_again_on_the_same_share() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1")];
        let (report, _, _, waits, left) = run_walk(&targets, 3, |id, attempt| match (id, attempt) {
            ("c1-p001-d0", 0) => Attempt::Refused(Failure::RateLimited.reason_key()),
            ("c1-p001-d0", 1) => Attempt::Refused(Failure::Unreachable.reason_key()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!(report.cleaned, 2);
        assert!(report.failed.is_empty() && report.stopped.is_none());
        assert_eq!(waits, RETRY_WAITS.to_vec());
        // Two regions, two shares, however many tries the first took.
        assert_eq!(left, Ok(1));
    }

    #[test]
    fn a_server_still_busy_after_every_wait_stops_the_batch() {
        let targets = [target(0, "c1-p001-d0"), target(1, "c1-p002-d0")];
        let (report, summary, _, waits, _) = run_walk(&targets, 2,
            |_, _| Attempt::Refused(Failure::RateLimited.reason_key()), None);
        assert_eq!(waits.len(), RETRY_WAITS.len());
        assert_eq!(report.cleaned, 0);
        assert_eq!(report.left, targets.to_vec());
        assert_eq!(report.stopped.as_deref(), Some(Failure::RateLimited.reason_key()));
        assert_eq!(summary.reason, "failed");
    }

    #[test]
    fn stop_ends_the_batch_and_leaves_the_rest() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(1, "c1-p002-d0")];
        let (report, summary, types, _, _) = run_walk(&targets, 3, cleaned, Some(1));
        assert_eq!(report.cleaned, 1);
        assert_eq!(report.left, vec![targets[1].clone(), targets[2].clone()]);
        assert!(report.by_user);
        assert_eq!(summary.reason, "cancelled");
        assert_eq!(types, ["page-started", "page-done"]);
        let Event::Notice { key, .. } = closing_notice("run-nb", &report) else { panic!("not a notice") };
        assert_eq!(key, "nanoBanana.job.stopped");
    }

    #[test]
    fn a_stop_during_a_wait_sends_no_retry() {
        let targets = [target(0, "c1-p001-d0")];
        let (report, _, _, waits, _) = run_walk(&targets, 1, |_, _| Attempt::Refused(Failure::Unreachable.reason_key()), Some(1));
        assert_eq!(waits.len(), 1, "the wait was cut short and nothing was sent after it");
        assert!(report.by_user);
        assert_eq!(report.left, targets.to_vec());
    }

    #[test]
    fn nothing_goes_past_what_the_grant_covers() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(0, "c1-p001-d2")];
        let (report, summary, _, _, _) = run_walk(&targets, 2, cleaned, None);
        assert_eq!(report.cleaned, 2);
        assert_eq!(report.left, vec![targets[2].clone()]);
        assert_eq!(report.stopped.as_deref(), Some(NO_CONSENT));
        assert!(!report.by_user, "a grant used up or replaced is not a Stop");
        assert_eq!(summary.reason, "cancelled");
        let Event::Notice { key, params, .. } = closing_notice("run-nb", &report) else { panic!("not a notice") };
        assert_eq!((key.as_str(), params["reasonKey"].as_str()), ("nanoBanana.job.halted", Some(NO_CONSENT)));
    }

    #[test]
    fn a_withdrawn_grant_sends_no_retry() {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(), 1).unwrap();
        let tries = Mutex::new(0);
        let clean = |_: &Target, _: &Config| {
            *tries.lock().unwrap() += 1;
            Attempt::Refused(Failure::RateLimited.reason_key())
        };
        // Stop pressed elsewhere while the batch waits.
        let sleep = |_: Duration, _: &AtomicBool| {
            book.revoke();
            true
        };
        let allowed = || true;
        let walk_with = Walk {
            run_id: "run-nb", chapter_id: "c1", grants: &book, nonce: &nonce, cloud_allowed: &allowed,
            clean: &clean, page_done: &page_done, sleep: &sleep, waits: &RETRY_WAITS,
        };
        let (report, _) = walk(&walk_with, &[target(0, "c1-p001-d0")], &AtomicBool::new(false), &|_| {});
        assert_eq!(*tries.lock().unwrap(), 1);
        assert_eq!(report.stopped.as_deref(), Some(NO_CONSENT));
    }

    #[test]
    fn a_region_gone_or_failing_to_write_is_counted_apart() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(0, "c1-p001-d2")];
        let (report, _, _, _, _) = run_walk(&targets, 3, |id, _| match id {
            "c1-p001-d0" => Attempt::Gone,
            "c1-p001-d1" => Attempt::Error("job_busy".into()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!((report.cleaned, report.gone), (1, 1));
        assert_eq!(report.failed, vec![unclean(&targets[1], ERROR_REASON)]);
        let Event::Notice { key, .. } = closing_notice("run-nb", &report) else { panic!("not a notice") };
        assert_eq!(key, "nanoBanana.job.finishedWithFailures");
    }

    #[test]
    fn the_regions_are_checked_before_anything_is_reserved() {
        assert_eq!(targets_of("c1", &[]).err().as_deref(), Some("nano_banana_no_regions"));
        assert!(targets_of("c1", &["c2-p001-d0".into()]).unwrap_err().starts_with("nano_banana_region_not_in_chapter"));
        let targets = targets_of("c1", &["c1-p003-d0".into(), "c1-p001-d4".into(), "c1-p003-d0".into(), "c1-p001-d1".into()]).unwrap();
        assert_eq!(targets, vec![target(0, "c1-p001-d4"), target(0, "c1-p001-d1"), target(2, "c1-p003-d0")]);
    }

    #[test]
    fn the_report_is_camel_case() {
        let report = Report {
            total: 2, cleaned: 1, gone: 0,
            failed: vec![unclean(&target(0, "c1-p001-d0"), Failure::Blocked.reason_key())],
            left: vec![], stopped: None, by_user: false,
        };
        let wire = serde_json::to_value(Event::NanoBananaReport { run_id: "run-nb".into(), chapter_id: "c1".into(), report }).unwrap();
        assert_eq!(wire["type"], "nano-banana-report");
        assert_eq!(wire["runId"], "run-nb");
        assert_eq!(wire["report"]["failed"][0], serde_json::json!({
            "regionId": "c1-p001-d0", "pageIndex": 0, "reason": "decline.reason.nanoBananaBlocked",
        }));
        assert_eq!(wire["report"]["byUser"], false);
        assert!(wire["report"].get("stopped").is_none());
    }
}
