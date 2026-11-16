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
//! hold does not refuse it and its undo steps join one step for the batch
//! (`history::record_clean`; an edit journaled between two of its cleans
//! starts a second step, see there).
//!
//! A page is raised (`page-started`) under the chapter's lock, as the run
//! walk raises one, and not while a single region's Nano Banana request is
//! out on it: that request let go of the lock and would find the page taken
//! when its paid answer came back (`region::nano_banana_requests_out`). The
//! batch waits for it, and is refused at the start while one is out on a
//! longstrip chapter, where it holds every page from the start.
//!
//! ## Consent
//!
//! The person confirmed a count and a cost in the window
//! (`nanobananaask.svelte.js`), which made a grant for that many regions of
//! this chapter (`nano_banana_consent.rs`). The batch is refused before it
//! starts unless that grant is live, is this chapter's, and covers every
//! region it was handed, and it claims the grant as its own then: no other
//! batch and no single region spends it after. Each region spends one share
//! just before its first request. A retry of the same region uses the same
//! share, so the count is never more than the window said, and it is asked
//! again for before each retry: Stop or Cloud engines turned off withdraws
//! it, and the retry does not go. The share carries the grant's stop, so a
//! Stop at any point after it was spent ends the request before it is sent.
//! Another Nano Banana window has a grant of its own and leaves this one
//! alone. When the batch ends, however it ends, a panic included, what is
//! left of its grant is withdrawn.
//!
//! ## What a failure means
//!
//! [`Failure`] says what Google answered, and that decides what the batch does:
//!
//! - **The account** ([`Verdict::Account`]): no key, a key store that will not
//!   answer, a rejected or expired key, a country Google does not serve,
//!   billing, a refused model, Cloud engines off, or a server that asked for
//!   a wait longer than any the batch makes. Every other region would fail
//!   the same way, so the batch stops at once and says why.
//! - **Busy** ([`Verdict::Busy`]): a rate limit, or Google not reachable
//!   before the request was sent. The region is tried again after a wait
//!   ([`RETRY_WAITS`]), and when the waits are used up the batch stops,
//!   because the next region would wait too.
//! - **The crop** ([`Verdict::Crop`]): a safety refusal, a recitation, an
//!   answer that could not be used, a request Google refused
//!   as put, a link lost after the whole request was sent (its answer may be
//!   billed, so it is not sent again), a page changed while the request was
//!   out, a region too large. Another region may well go through, so this
//!   one is skipped with its reason and the batch goes on.
//! - **Stopped** ([`Verdict::Stopped`]): Stop, or the grant withdrawn or
//!   lapsed. The batch ends as cancelled.
//!
//! ## Generations
//!
//! With Generations per region above one (`variations.rs#GENERATIONS_KEY`,
//! sent with the batch), each region the batch cleans is asked again that
//! many times less one, each on a share of its own: the window counted
//! requests, regions times generations, and the grant is for that many. The
//! first answer is the region's result and its undo step, as at one
//! generation; the others are kept as its variations only
//! (`region::variation_for_nano_banana_batch`). An answer the region already
//! has is no new variation and is not asked again. A further answer that
//! fails leaves the region cleaned and the batch going, unless it failed for
//! a reason that stops a batch; Stop ends them too. Each one not made is
//! counted in the report ([`Report::generations_not_made`]).
//!
//! At the end the batch sends its [`Report`] (`nano-banana-report`) and one
//! notice. The interface shows the counts per reason from the report, and can
//! start a new batch for the regions it did not clean, behind a new window.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

use super::nano_banana::{Failure, Granted};
use super::nano_banana_consent::{Book, CLOUD_OFF, MAX_REGIONS, NO_CONSENT};
use crate::events::{self, Event};
use crate::library::{self, ApiRegion, Library};
use crate::run::{self, QueuedPage, RunHandle, Summary};

/// The waits before each retry of a region that met a rate limit or could not
/// reach Google. The request itself already waited once, up to a minute, for a
/// server that named a wait (`nano_banana.rs#refused`). Two more tries, then
/// the batch stops. A server that names a longer wait than a minute is
/// [`Failure::LongWait`], which stops the batch at once: these waits are
/// shorter.
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
            Failure::Location, Failure::Billing, Failure::ModelRejected, Failure::LongWait, Failure::NoWindow])
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
    /// Further generations of the regions cleaned that were asked for and
    /// not kept: refused, failed, stopped, or an answer the region had.
    #[serde(default)]
    pub generations_not_made: u32,
}

/// One region's clean, as the batch sees it.
pub(crate) enum Attempt {
    Cleaned(Option<Box<ApiRegion>>),
    Gone,
    Refused(&'static str),
    Error(String),
    /// A further generation that answered with an answer the region already
    /// had: no new variation.
    Repeated,
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
    /// Raise the page: send its `page-started` with the chapter's lock held,
    /// once no single region's request is out on it. Answers false when
    /// `cancel` went up while it waited, and then sends nothing.
    pub raise: &'a dyn Fn(u32, &AtomicBool, &dyn Fn(Event)) -> bool,
    /// Clean one region under what its share granted.
    pub clean: &'a dyn Fn(&Target, &Granted) -> Attempt,
    /// Ask once more for a region cleaned in this batch, under what another
    /// share granted, and keep the answer as a variation only.
    pub vary: &'a dyn Fn(&Target, &Granted) -> Attempt,
    /// Generations per region: the first is `clean`'s, the rest `vary`'s.
    pub generations: u32,
    /// The page's `page-done`, as the manifest now holds it.
    pub page_done: &'a dyn Fn(u32) -> Event,
    /// Wait this long, or less when `cancel` goes up. Answers whether the
    /// whole wait passed.
    pub sleep: &'a dyn Fn(Duration, &AtomicBool) -> bool,
    pub waits: &'a [Duration],
}

/// Walk `targets` page by page. Answers the report and the run's summary.
#[cfg(test)]
pub(crate) fn walk(walk: &Walk<'_>, targets: &[Target], cancel: &AtomicBool, emit: &dyn Fn(Event))
    -> (Report, Summary) {
    let mut report = Report::default();
    let summary = walk_into(&mut report, walk, targets, cancel, emit);
    (report, summary)
}

/// [`walk`], into `report` as it goes: every target starts in `left` and
/// leaves it when it is handled, so a walk that panics leaves a report that
/// is true as far as it got.
pub(crate) fn walk_into(report: &mut Report, walk: &Walk<'_>, targets: &[Target], cancel: &AtomicBool,
    emit: &dyn Fn(Event)) -> Summary {
    let mut pages: BTreeMap<u32, Vec<&Target>> = BTreeMap::new();
    for target in targets {
        pages.entry(target.page_index).or_default().push(target);
    }
    *report = Report {
        total: u32::try_from(targets.len()).unwrap_or(u32::MAX),
        left: pages.values().flatten().map(|target| (*target).clone()).collect(),
        ..Report::default()
    };
    let handled = |report: &mut Report, target: &Target| {
        if let Some(at) = report.left.iter().position(|left| left == target) {
            report.left.remove(at);
        }
    };
    let mut pages_done = 0u32;
    let mut errored = 0u32;
    let mut stopped: Option<&'static str> = None;
    for (page, on_page) in &pages {
        if stopped.is_none() && cancel.load(Ordering::SeqCst) {
            stopped = Some(Failure::Stopped.reason_key());
        }
        if stopped.is_some() {
            break;
        }
        if !(walk.raise)(*page, cancel, emit) {
            stopped = Some(Failure::Stopped.reason_key());
            break;
        }
        let mut page_errored = false;
        for target in on_page {
            if stopped.is_none() && cancel.load(Ordering::SeqCst) {
                stopped = Some(Failure::Stopped.reason_key());
            }
            if stopped.is_some() {
                break;
            }
            match clean_one(walk, target, cancel, walk.clean) {
                Attempt::Cleaned(region) => {
                    report.cleaned += 1;
                    handled(report, target);
                    if let Some(region) = region {
                        emit(Event::RegionDone {
                            run_id: walk.run_id.to_owned(),
                            chapter_id: walk.chapter_id.to_owned(),
                            page_id: library::page_id(walk.chapter_id, target.page_index as usize),
                            page_index: target.page_index,
                            region,
                        });
                    }
                    // The region is cleaned with its first answer; the others
                    // are its variations, and a failed one fails nothing.
                    let wanted = walk.generations.saturating_sub(1);
                    for asked in 0..wanted {
                        if cancel.load(Ordering::SeqCst) {
                            stopped = Some(Failure::Stopped.reason_key());
                        }
                        if stopped.is_some() {
                            report.generations_not_made += wanted - asked;
                            break;
                        }
                        match clean_one(walk, target, cancel, walk.vary) {
                            Attempt::Cleaned(_) => {}
                            Attempt::Refused(reason) => {
                                report.generations_not_made += 1;
                                let reason = if reason == Failure::Stopped.reason_key() && !(walk.cloud_allowed)() {
                                    CLOUD_OFF
                                } else {
                                    reason
                                };
                                if verdict(reason) != Verdict::Crop {
                                    stopped = Some(reason);
                                }
                            }
                            Attempt::Error(error) => {
                                eprintln!("manga-cleaner: a Nano Banana batch could not keep a variation of {}: {error}",
                                    target.region_id);
                                report.generations_not_made += 1;
                            }
                            Attempt::Gone | Attempt::Repeated => report.generations_not_made += 1,
                        }
                    }
                }
                // Only a further generation answers this.
                Attempt::Repeated => handled(report, target),
                Attempt::Gone => {
                    report.gone += 1;
                    handled(report, target);
                }
                Attempt::Error(error) => {
                    eprintln!("manga-cleaner: a Nano Banana batch could not clean {}: {error}", target.region_id);
                    report.failed.push(unclean(target, ERROR_REASON));
                    handled(report, target);
                    page_errored = true;
                }
                Attempt::Refused(reason) => {
                    // A stop that came with Cloud engines turned off is that.
                    let reason = if reason == Failure::Stopped.reason_key() && !(walk.cloud_allowed)() {
                        CLOUD_OFF
                    } else {
                        reason
                    };
                    match verdict(reason) {
                        Verdict::Crop => {
                            report.failed.push(unclean(target, reason));
                            handled(report, target);
                        }
                        Verdict::Stopped | Verdict::Account | Verdict::Busy => stopped = Some(reason),
                    }
                }
            }
        }
        emit((walk.page_done)(*page));
        // A page the batch stopped inside is not a page cleaned.
        if stopped.is_none() {
            pages_done += 1;
        }
        if page_errored {
            errored += 1;
        }
    }
    report.by_user = stopped == Some(Failure::Stopped.reason_key());
    report.stopped = stopped.map(str::to_owned);
    let reason = match stopped.map(verdict) {
        None => "completed",
        Some(Verdict::Stopped) => "cancelled",
        Some(_) => "failed",
    };
    Summary {
        reason,
        pages_queued: u32::try_from(pages.len()).unwrap_or(u32::MAX),
        pages_cleaned: pages_done,
        regions_cleaned: report.cleaned,
        next_page_index: None,
        errored,
        regions_detected: 0,
        regions_declined: u32::try_from(report.failed.len()).unwrap_or(u32::MAX),
    }
}

/// What a batch whose walk panicked reports: what `report` says it did, the
/// region it was on among the ones left, and the error as why it stopped.
fn after_panic(report: &mut Report, pages_queued: u32) -> Summary {
    report.stopped = Some(ERROR_REASON.to_owned());
    report.by_user = false;
    Summary {
        reason: "failed",
        pages_queued,
        pages_cleaned: 0,
        regions_cleaned: report.cleaned,
        next_page_index: None,
        errored: 1,
        regions_detected: 0,
        regions_declined: u32::try_from(report.failed.len()).unwrap_or(u32::MAX),
    }
}

fn unclean(target: &Target, reason: &str) -> Unclean {
    Unclean { region_id: target.region_id.clone(), page_index: target.page_index, reason: reason.to_owned() }
}

/// One region: its share of the grant, then the clean, tried again after each
/// wait while Google is busy, as long as the grant still holds.
fn clean_one(walk: &Walk<'_>, target: &Target, cancel: &AtomicBool, clean: &dyn Fn(&Target, &Granted) -> Attempt)
    -> Attempt {
    let spend = match walk.grants.spend((walk.cloud_allowed)(), Some(walk.nonce), walk.chapter_id, Some(walk.run_id)) {
        Ok(spend) => spend,
        Err(reason) => return Attempt::Refused(reason),
    };
    let granted = spend.granted();
    let mut waits = walk.waits.iter();
    loop {
        let attempt = run::on_behalf_of(walk.run_id, || clean(target, &granted));
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
/// more requests, `generations` per region, than a grant can cover. Sorted by
/// page, in the order given within one.
fn targets_of(chapter_id: &str, region_ids: &[String], generations: u32) -> Result<Vec<Target>, String> {
    if region_ids.is_empty() {
        return Err("nano_banana_no_regions".into());
    }
    if region_ids.len().saturating_mul(generations as usize) > MAX_REGIONS as usize {
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
    if targets.len().saturating_mul(generations as usize) > MAX_REGIONS as usize {
        return Err(NO_CONSENT.into());
    }
    Ok(targets)
}

/// Why a batch did not start: a single region's Nano Banana request is out on
/// its chapter. A catalogue key, as the interface shows it.
pub const REQUEST_OUT: &str = "nanoBanana.job.requestOut";

/// Start the batch, or answer with the run already holding the chapter.
/// Refused before anything is reserved while Cloud engines are off and when
/// the grant `nano_banana_grant` is not live, is another chapter's, or covers
/// fewer requests than it was handed, `generations` per region; refused with
/// [`REQUEST_OUT`] while a single region's request is out on the chapter.
fn start(app: &tauri::AppHandle, chapter_id: &str, region_ids: &[String], nano_banana_grant: Option<&str>,
    generations: u32) -> Result<RunHandle, String> {
    let grants = super::nano_banana_consent::book();
    let nonce = nano_banana_grant.ok_or(NO_CONSENT)?.to_owned();
    let targets = targets_of(chapter_id, region_ids, generations)?;
    let requests = targets.len() * generations as usize;
    let cloud_allowed = super::cloud_allowed(app);
    let left = grants.left(cloud_allowed, &nonce)?;
    if (left as usize) < requests {
        return Err(NO_CONSENT.into());
    }
    let reservation = match run::reserve_run(chapter_id, &[], run::RunKind::NanoBanana)? {
        Ok(reservation) => reservation,
        Err(busy) => return Ok(busy),
    };
    let library = Library::for_app(app).map_err(|e| e.to_string())?;
    let job_path = library.resolve_chapter(chapter_id).map_err(|e| e.to_string())?;
    reservation.set_jobs([&job_path]);
    {
        // Under the lock such a request let go of, so one sent before this
        // is seen, and one asked for after it meets the run.
        let _lock = run::lock_job(&job_path).ok();
        if crate::region::nano_banana_requests_out(&job_path, None) {
            return Err(REQUEST_OUT.into());
        }
    }
    grants.claim(cloud_allowed, &nonce, chapter_id, reservation.run_id(), requests)?;
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
        // However the batch ends, a panic included, what is left of its grant
        // goes with it.
        struct Withdraw<'a>(&'a str);
        impl Drop for Withdraw<'_> {
            fn drop(&mut self) {
                super::nano_banana_consent::book().revoke_owned_by(self.0);
            }
        }
        let _withdraw = Withdraw(run_id);
        let clean = |target: &Target, granted: &Granted| {
            match crate::region::clean_for_nano_banana_batch(&app, &library, &target.region_id, granted) {
                Ok(crate::region::BatchRegion::Cleaned(region)) => Attempt::Cleaned(Some(region)),
                Ok(crate::region::BatchRegion::Gone) => Attempt::Gone,
                Ok(crate::region::BatchRegion::Refused(reason)) => Attempt::Refused(reason),
                Err(error) => Attempt::Error(error),
            }
        };
        let vary = |target: &Target, granted: &Granted| {
            match crate::region::variation_for_nano_banana_batch(&app, &library, &target.region_id, granted) {
                Ok(crate::region::BatchVariation::Kept) => Attempt::Cleaned(None),
                Ok(crate::region::BatchVariation::Repeated) => Attempt::Repeated,
                Ok(crate::region::BatchVariation::Gone) => Attempt::Gone,
                Ok(crate::region::BatchVariation::Refused(reason)) => Attempt::Refused(reason),
                Err(error) => Attempt::Error(error),
            }
        };
        let page_done = |page: u32| run::page_done_after_clean(run_id, &chapter, &job_path, page);
        let raise = |page: u32, cancel: &AtomicBool, emit: &dyn Fn(Event)| raise_page(run_id, &chapter, &job_path, page, cancel, emit);
        let cloud_allowed = || super::cloud_allowed(&app);
        let walk_with = Walk {
            run_id, chapter_id: &chapter, grants, nonce: &nonce, cloud_allowed: &cloud_allowed, raise: &raise,
            clean: &clean, vary: &vary, generations, page_done: &page_done, sleep: &sleep_unless_cancelled,
            waits: &RETRY_WAITS,
        };
        let mut report = Report::default();
        let walked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            walk_into(&mut report, &walk_with, &targets, cancel, emit)
        }));
        let summary = walked.unwrap_or_else(|_| {
            eprintln!("manga-cleaner: a Nano Banana batch stopped on a panic");
            let pages = targets.iter().map(|target| target.page_index).collect::<BTreeSet<_>>().len();
            after_panic(&mut report, u32::try_from(pages).unwrap_or(u32::MAX))
        });
        finish(run_id, &chapter, &report, emit);
        summary
    }, run::Closing::Body))
}

/// Send `page`'s `page-started` with the chapter's lock held, as the run walk
/// raises a page, once no single region's Nano Banana request is out on it
/// (`region::nano_banana_requests_out`): that request let go of the lock, and
/// its paid answer would find the page taken. Answers false, having sent
/// nothing, when `cancel` goes up first.
pub(crate) fn raise_page(run_id: &str, chapter_id: &str, job_path: &std::path::Path, page: u32, cancel: &AtomicBool,
    emit: &dyn Fn(Event)) -> bool {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        // Without the lock, because another process holds the chapter, the
        // page is raised all the same, as `page_done_after_clean` answers one.
        let lock = run::lock_job(job_path).ok();
        if !crate::region::nano_banana_requests_out(job_path, Some(page as usize)) {
            emit(run::page_started_event(run_id, chapter_id, page));
            drop(lock);
            return true;
        }
        drop(lock);
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The report, then the closing notice, before the run's `run-finished`.
fn finish(run_id: &str, chapter_id: &str, report: &Report, emit: &dyn Fn(Event)) {
    emit(Event::NanoBananaReport { run_id: run_id.to_owned(), chapter_id: chapter_id.to_owned(), report: report.clone() });
    emit(closing_notice(run_id, report));
}

/// `startNanoBananaRun`: clean `region_ids`, stored detections of
/// `chapter_id`, with Nano Banana, on the grant the window just made, making
/// `generations` of each (Generations per region, 1 when not sent; a count
/// out of range is refused). Answers the run's handle as `runClean` does,
/// `alreadyRunning` included.
#[tauri::command]
pub async fn start_nano_banana_run(
    app: tauri::AppHandle,
    chapter_id: String,
    region_ids: Vec<String>,
    nano_banana_grant: Option<String>,
    generations: Option<serde_json::Value>,
) -> Result<RunHandle, String> {
    let generations = match generations {
        None | Some(serde_json::Value::Null) => crate::variations::DEFAULT_GENERATIONS,
        Some(value) => crate::variations::generations_from_value(&value)?,
    };
    tauri::async_runtime::spawn_blocking(move || {
        start(&app, &chapter_id, &region_ids, nano_banana_grant.as_deref(), generations)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::nano_banana::Config;
    use crate::inference::nano_banana_consent::IDLE;
    use cleaner_core::engines::nano_banana::Model;
    use std::cell::RefCell;
    use std::sync::Mutex;

    fn config() -> Config {
        Config { model: Model::DEFAULT, instruction: None, restrictive: false }
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
        run_walk_on(&Book::new(IDLE), targets, granted, answer, cancel_after)
    }

    /// The test grants: `granted` regions of `c1`, claimed by the batch.
    fn claimed(book: &Book, granted: u32) -> String {
        let nonce = book.issue(config(), "c1", granted).unwrap();
        book.claim(true, &nonce, "c1", "run-nb", 0).unwrap();
        nonce
    }

    /// Raise every page at once, as no request is out.
    fn raise(page: u32, _: &AtomicBool, emit: &dyn Fn(Event)) -> bool {
        emit(run::page_started_event("run-nb", "c1", page));
        true
    }

    fn run_walk_on(
        book: &Book,
        targets: &[Target],
        granted: u32,
        answer: impl Fn(&str, usize) -> Attempt,
        cancel_after: Option<usize>,
    ) -> (Report, Summary, Vec<String>, Vec<Duration>, Result<u32, &'static str>) {
        let nonce = claimed(book, granted);
        let tries = Mutex::new(Vec::<String>::new());
        let cancel = AtomicBool::new(false);
        let clean = |target: &Target, _: &Granted| {
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
            run_id: "run-nb", chapter_id: "c1", grants: book, nonce: &nonce, cloud_allowed: &allowed, raise: &raise,
            clean: &clean, vary: &no_variation, generations: 1, page_done: &page_done, sleep: &sleep, waits: &RETRY_WAITS,
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

    /// A batch at one generation asks for none.
    fn no_variation(_: &Target, _: &Granted) -> Attempt {
        panic!("a batch at one generation asked for a variation")
    }

    #[test]
    fn every_failure_has_its_verdict() {
        for failure in [Failure::NoKey, Failure::KeyStore, Failure::KeyRejected, Failure::KeyExpired,
            Failure::Location, Failure::Billing, Failure::ModelRejected, Failure::LongWait] {
            assert_eq!(verdict(failure.reason_key()), Verdict::Account, "{failure:?}");
        }
        assert_eq!(verdict(CLOUD_OFF), Verdict::Account);
        for failure in [Failure::RateLimited, Failure::Unreachable] {
            assert_eq!(verdict(failure.reason_key()), Verdict::Busy, "{failure:?}");
        }
        for failure in [Failure::Blocked, Failure::Recited, Failure::BadAnswer, Failure::Lost,
            Failure::BadRequest] {
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
            "c1-p002-d0" => Attempt::Refused(Failure::BadAnswer.reason_key()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!(report.cleaned, 2);
        assert_eq!(report.failed, vec![
            unclean(&targets[0], Failure::Blocked.reason_key()),
            unclean(&targets[2], Failure::BadAnswer.reason_key()),
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
        assert_eq!(summary.pages_cleaned, 0, "the page it stopped inside is not a page cleaned");
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
        let nonce = claimed(&book, 1);
        let tries = Mutex::new(0);
        let clean = |_: &Target, _: &Granted| {
            *tries.lock().unwrap() += 1;
            Attempt::Refused(Failure::RateLimited.reason_key())
        };
        // Stop pressed while the batch waits.
        let sleep = |_: Duration, _: &AtomicBool| {
            book.revoke_owned_by("run-nb");
            true
        };
        let allowed = || true;
        let walk_with = Walk {
            run_id: "run-nb", chapter_id: "c1", grants: &book, nonce: &nonce, cloud_allowed: &allowed, raise: &raise,
            clean: &clean, vary: &no_variation, generations: 1, page_done: &page_done, sleep: &sleep, waits: &RETRY_WAITS,
        };
        let (report, _) = walk(&walk_with, &[target(0, "c1-p001-d0")], &AtomicBool::new(false), &|_| {});
        assert_eq!(*tries.lock().unwrap(), 1);
        assert_eq!(report.stopped.as_deref(), Some(NO_CONSENT));
    }

    #[test]
    fn a_region_gone_or_failing_to_write_is_counted_apart() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(0, "c1-p001-d2")];
        let (report, summary, _, _, _) = run_walk(&targets, 3, |id, _| match id {
            "c1-p001-d0" => Attempt::Gone,
            "c1-p001-d1" => Attempt::Error("job_busy".into()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!((report.cleaned, report.gone), (1, 1));
        assert_eq!(report.failed, vec![unclean(&targets[1], ERROR_REASON)]);
        assert_eq!(summary.errored, 1, "the page a region failed to write on");
        assert_eq!(summary.pages_cleaned, 1);
        let Event::Notice { key, .. } = closing_notice("run-nb", &report) else { panic!("not a notice") };
        assert_eq!(key, "nanoBanana.job.finishedWithFailures");
    }

    #[test]
    fn the_regions_are_checked_before_anything_is_reserved() {
        assert_eq!(targets_of("c1", &[], 1).err().as_deref(), Some("nano_banana_no_regions"));
        assert!(targets_of("c1", &["c2-p001-d0".into()], 1).unwrap_err().starts_with("nano_banana_region_not_in_chapter"));
        let targets = targets_of("c1", &["c1-p003-d0".into(), "c1-p001-d4".into(), "c1-p003-d0".into(), "c1-p001-d1".into()], 1).unwrap();
        assert_eq!(targets, vec![target(0, "c1-p001-d4"), target(0, "c1-p001-d1"), target(2, "c1-p003-d0")]);
    }

    /// The limit a grant can cover is on requests, regions times generations.
    #[test]
    fn the_request_limit_counts_every_generation() {
        let ids = |count: u32| (0..count).map(|at| format!("c1-p001-d{at}")).collect::<Vec<_>>();
        assert!(targets_of("c1", &ids(MAX_REGIONS), 1).is_ok());
        assert_eq!(targets_of("c1", &ids(MAX_REGIONS / 2 + 1), 2).err().as_deref(), Some(NO_CONSENT));
        assert!(targets_of("c1", &ids(MAX_REGIONS / 2), 2).is_ok());
    }

    /// The walk at `generations`, every first clean answered `Cleaned`, and
    /// each further one by `vary`, with a grant for `granted` requests.
    /// Answers the report, the summary, the event types, what is left of the
    /// grant, and the variations asked for by region.
    fn run_generations(
        targets: &[Target],
        granted: u32,
        generations: u32,
        vary: impl Fn(&str, usize) -> Attempt,
        cancel_after_variations: Option<usize>,
    ) -> (Report, Summary, Vec<String>, Result<u32, &'static str>, Vec<String>) {
        let book = Book::new(IDLE);
        let nonce = claimed(&book, granted);
        let cancel = AtomicBool::new(false);
        let asked = Mutex::new(Vec::<String>::new());
        let clean = |_: &Target, _: &Granted| Attempt::Cleaned(None);
        let vary_one = |target: &Target, _: &Granted| {
            let mut seen = asked.lock().unwrap();
            let attempt = seen.iter().filter(|id| **id == target.region_id).count();
            seen.push(target.region_id.clone());
            if cancel_after_variations.is_some_and(|after| seen.len() >= after) {
                cancel.store(true, Ordering::SeqCst);
            }
            vary(&target.region_id, attempt)
        };
        let events = RefCell::new(Vec::new());
        let emit = |event: Event| events.borrow_mut().push(serde_json::to_value(&event).unwrap()["type"].as_str().unwrap().to_owned());
        let allowed = || true;
        let sleep = |_: Duration, cancel: &AtomicBool| !cancel.load(Ordering::SeqCst);
        let walk_with = Walk {
            run_id: "run-nb", chapter_id: "c1", grants: &book, nonce: &nonce, cloud_allowed: &allowed, raise: &raise,
            clean: &clean, vary: &vary_one, generations, page_done: &page_done, sleep: &sleep, waits: &RETRY_WAITS,
        };
        let (report, summary) = walk(&walk_with, targets, &cancel, &emit);
        let left = book.left(true, &nonce);
        (report, summary, events.into_inner(), left, asked.into_inner().unwrap())
    }

    /// At two generations each region spends two shares, one for its first
    /// answer and one for its variation, and the region counts as cleaned
    /// once, with its first.
    #[test]
    fn a_batch_at_two_generations_spends_two_shares_per_region() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(1, "c1-p002-d0")];
        let (report, summary, types, left, asked) = run_generations(&targets, 7, 2, |_, _| Attempt::Cleaned(None), None);
        assert_eq!((report.cleaned, report.generations_not_made), (3, 0));
        assert_eq!(summary.regions_cleaned, 3, "a region counts once, with its first answer");
        assert_eq!(left, Ok(1), "three regions, two requests each, of a grant for seven");
        assert_eq!(asked, ["c1-p001-d0", "c1-p001-d1", "c1-p002-d0"]);
        assert_eq!(types, ["page-started", "page-done", "page-started", "page-done"]);
        assert_eq!(summary.reason, "completed");
    }

    /// A further answer that is refused, that fails, or that the region had
    /// already leaves the region cleaned and the batch going, counted as not
    /// made. One refused for a reason that stops a batch stops it, the region
    /// still cleaned.
    #[test]
    fn a_failed_variation_keeps_the_region_cleaned() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(0, "c1-p001-d2")];
        let (report, summary, _, _, _) = run_generations(&targets, 9, 3, |id, attempt| match (id, attempt) {
            ("c1-p001-d0", 0) => Attempt::Refused(Failure::Blocked.reason_key()),
            ("c1-p001-d1", 0) => Attempt::Repeated,
            ("c1-p001-d1", 1) => Attempt::Error("disk full".into()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!((report.cleaned, report.generations_not_made), (3, 3));
        assert!(report.failed.is_empty() && report.left.is_empty() && report.stopped.is_none());
        assert_eq!(summary.reason, "completed");

        let (report, summary, _, _, asked) = run_generations(&targets, 9, 3, |id, _| match id {
            "c1-p001-d1" => Attempt::Refused(Failure::KeyRejected.reason_key()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!(report.cleaned, 2, "the region whose variation stopped the batch is cleaned");
        assert_eq!(report.generations_not_made, 2);
        assert_eq!(report.left, vec![targets[2].clone()]);
        assert_eq!(report.stopped.as_deref(), Some(Failure::KeyRejected.reason_key()));
        assert_eq!(summary.reason, "failed");
        assert_eq!(asked, ["c1-p001-d0", "c1-p001-d0", "c1-p001-d1"]);
    }

    /// Stop between two variations ends them and the batch; the region it
    /// was on stays cleaned, and what it did not ask is counted.
    #[test]
    fn stop_ends_the_variations_too() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1")];
        let (report, summary, _, left, asked) = run_generations(&targets, 8, 4, |_, _| Attempt::Cleaned(None), Some(1));
        assert_eq!(asked.len(), 1);
        assert_eq!((report.cleaned, report.generations_not_made), (1, 2));
        assert_eq!(report.left, vec![targets[1].clone()]);
        assert!(report.by_user);
        assert_eq!(summary.reason, "cancelled");
        assert_eq!(left, Ok(6), "one first answer and one variation spent");
    }

    #[test]
    fn a_link_lost_after_the_request_was_sent_skips_the_region_and_sends_it_no_more() {
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1")];
        let (report, summary, _, waits, _) = run_walk(&targets, 2, |id, _| match id {
            "c1-p001-d0" => Attempt::Refused(Failure::Lost.reason_key()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert!(waits.is_empty(), "a request that may be billed was sent again");
        assert_eq!(report.failed, vec![unclean(&targets[0], Failure::Lost.reason_key())]);
        assert_eq!((report.cleaned, summary.reason), (1, "completed"));
    }

    #[test]
    fn a_request_refused_as_put_skips_the_region_and_the_batch_goes_on() {
        let targets = [target(0, "c1-p001-d0"), target(1, "c1-p002-d0")];
        let (report, summary, _, _, _) = run_walk(&targets, 2, |id, _| match id {
            "c1-p001-d0" => Attempt::Refused(Failure::BadRequest.reason_key()),
            _ => Attempt::Cleaned(None),
        }, None);
        assert_eq!(report.cleaned, 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(summary.reason, "completed");
    }

    #[test]
    fn a_long_server_wait_stops_the_batch_without_waiting() {
        let targets = [target(0, "c1-p001-d0"), target(1, "c1-p002-d0")];
        let (report, summary, _, waits, _) = run_walk(&targets, 2,
            |_, _| Attempt::Refused(Failure::LongWait.reason_key()), None);
        assert!(waits.is_empty());
        assert_eq!(report.left, targets.to_vec());
        assert_eq!(report.stopped.as_deref(), Some(Failure::LongWait.reason_key()));
        assert_eq!(summary.reason, "failed");
    }

    #[test]
    fn a_region_that_outlasts_the_idle_lapse_may_still_be_retried() {
        let book = Book::new(Duration::from_millis(50));
        let targets = [target(0, "c1-p001-d0")];
        let (report, _, _, waits, _) = run_walk_on(&book, &targets, 1, |_, attempt| {
            if attempt == 0 {
                // Longer than the grant may sit idle.
                std::thread::sleep(Duration::from_millis(120));
                Attempt::Refused(Failure::RateLimited.reason_key())
            } else {
                Attempt::Cleaned(None)
            }
        }, None);
        assert_eq!(waits.len(), 1);
        assert_eq!(report.cleaned, 1, "the retry was refused as no consent: {:?}", report.stopped);
    }

    #[test]
    fn another_window_does_not_halt_the_batch() {
        let book = Book::new(IDLE);
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1")];
        let (report, summary, _, _, _) = run_walk_on(&book, &targets, 2, |_, _| {
            // A single region confirmed elsewhere while the batch runs.
            let _ = book.issue(config(), "c2", 1).unwrap();
            Attempt::Cleaned(None)
        }, None);
        assert_eq!((report.cleaned, summary.reason), (2, "completed"));
    }

    #[test]
    fn a_stop_with_cloud_engines_turned_off_is_not_the_persons_stop() {
        let book = Book::new(IDLE);
        let nonce = claimed(&book, 1);
        let clean = |_: &Target, _: &Granted| Attempt::Refused(Failure::Stopped.reason_key());
        // On when the share was spent, off by the time the request stopped.
        let asked = std::cell::Cell::new(0);
        let allowed = || {
            asked.set(asked.get() + 1);
            asked.get() == 1
        };
        let walk_with = Walk {
            run_id: "run-nb", chapter_id: "c1", grants: &book, nonce: &nonce, cloud_allowed: &allowed, raise: &raise,
            clean: &clean, vary: &no_variation, generations: 1, page_done: &page_done, sleep: &|_, _| true, waits: &RETRY_WAITS,
        };
        let (report, summary) = walk(&walk_with, &[target(0, "c1-p001-d0")], &AtomicBool::new(false), &|_| {});
        assert_eq!(report.stopped.as_deref(), Some(CLOUD_OFF));
        assert!(!report.by_user);
        assert_eq!(summary.reason, "failed");
    }

    #[test]
    fn a_batch_spends_only_a_grant_it_claimed_on_its_own_chapter() {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(), "c2", 1).unwrap();
        assert_eq!(book.claim(true, &nonce, "c1", "run-nb", 1), Err(NO_CONSENT));
        let clean = |_: &Target, _: &Granted| Attempt::Cleaned(None);
        let walk_with = Walk {
            run_id: "run-nb", chapter_id: "c1", grants: &book, nonce: &nonce, cloud_allowed: &|| true, raise: &raise,
            clean: &clean, vary: &no_variation, generations: 1, page_done: &page_done, sleep: &|_, _| true, waits: &RETRY_WAITS,
        };
        let (report, _) = walk(&walk_with, &[target(0, "c1-p001-d0")], &AtomicBool::new(false), &|_| {});
        assert_eq!((report.cleaned, report.stopped.as_deref()), (0, Some(NO_CONSENT)));
    }

    #[test]
    fn a_page_not_raised_because_of_a_stop_sends_nothing() {
        let book = Book::new(IDLE);
        let nonce = claimed(&book, 1);
        let clean = |_: &Target, _: &Granted| -> Attempt { panic!("a region of an unraised page was cleaned") };
        // A single region's request held the page until Stop.
        let held = |_: u32, _: &AtomicBool, _: &dyn Fn(Event)| false;
        let walk_with = Walk {
            run_id: "run-nb", chapter_id: "c1", grants: &book, nonce: &nonce, cloud_allowed: &|| true, raise: &held,
            clean: &clean, vary: &no_variation, generations: 1, page_done: &page_done, sleep: &|_, _| true, waits: &RETRY_WAITS,
        };
        let events = RefCell::new(0);
        let (report, summary) = walk(&walk_with, &[target(0, "c1-p001-d0")], &AtomicBool::new(false), &|_| *events.borrow_mut() += 1);
        assert_eq!(*events.borrow(), 0);
        assert!(report.by_user);
        assert_eq!(summary.pages_cleaned, 0);
    }

    #[test]
    fn a_walk_that_panics_still_reports_what_it_did() {
        let book = Book::new(IDLE);
        let nonce = claimed(&book, 3);
        let targets = [target(0, "c1-p001-d0"), target(0, "c1-p001-d1"), target(1, "c1-p002-d0")];
        let clean = |target: &Target, _: &Granted| match target.region_id.as_str() {
            "c1-p001-d0" => Attempt::Cleaned(None),
            _ => panic!("a bug"),
        };
        let walk_with = Walk {
            run_id: "run-nb", chapter_id: "c1", grants: &book, nonce: &nonce, cloud_allowed: &|| true, raise: &raise,
            clean: &clean, vary: &no_variation, generations: 1, page_done: &page_done, sleep: &|_, _| true, waits: &RETRY_WAITS,
        };
        let mut report = Report::default();
        let walked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            walk_into(&mut report, &walk_with, &targets, &AtomicBool::new(false), &|_| {})
        }));
        assert!(walked.is_err());
        let summary = after_panic(&mut report, 2);
        assert_eq!(report.cleaned, 1);
        assert_eq!(report.left, vec![targets[1].clone(), targets[2].clone()], "the region it was on and the rest");
        assert_eq!(report.stopped.as_deref(), Some(ERROR_REASON));
        assert!(!report.by_user);
        assert_eq!((summary.reason, summary.errored, summary.regions_cleaned), ("failed", 1, 1));
        let Event::Notice { key, .. } = closing_notice("run-nb", &report) else { panic!("not a notice") };
        assert_eq!(key, "nanoBanana.job.halted");
    }

    #[test]
    fn the_report_is_camel_case() {
        let report = Report {
            total: 2, cleaned: 1, gone: 0,
            failed: vec![unclean(&target(0, "c1-p001-d0"), Failure::Blocked.reason_key())],
            left: vec![], stopped: None, by_user: false, generations_not_made: 2,
        };
        let wire = serde_json::to_value(Event::NanoBananaReport { run_id: "run-nb".into(), chapter_id: "c1".into(), report }).unwrap();
        assert_eq!(wire["type"], "nano-banana-report");
        assert_eq!(wire["runId"], "run-nb");
        assert_eq!(wire["report"]["failed"][0], serde_json::json!({
            "regionId": "c1-p001-d0", "pageIndex": 0, "reason": "decline.reason.nanoBananaBlocked",
        }));
        assert_eq!(wire["report"]["byUser"], false);
        assert_eq!(wire["report"]["generationsNotMade"], 2);
        assert!(wire["report"].get("stopped").is_none());
    }
}
