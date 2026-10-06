//! Submit-then-poll for analysis tiles and denoise pages
//! (`cleaner_core::cloud_job_wire`, `deploy/cloud/common/api.py` `/jobs` routes).
//!
//! Modal answers a web request still running after 150 s with a 303 redirect,
//! and [`crate::inference::http`] follows no redirect, so a tile or page that
//! queues or runs that long used to fail here as `gateway_unreachable` while
//! the GPU went on and billed it. [`run_job`] submits the work, which the
//! gateway answers at once with a handle, then polls that handle until the job
//! ends. Its invariants:
//!
//! - **No request is long.** Every request carries its own timeout, each well
//!   under the provider's redirect ([`SUBMIT_TIMEOUT`], [`STATUS_TIMEOUT`],
//!   [`CANCEL_TIMEOUT`], checked at compile time), and the job as a whole has
//!   the [`Schedule`] deadline instead.
//! - **A submit is idempotent.** The handle is derived from the request digest
//!   and an attempt id fixed for the call, so a submit repeated after a lost
//!   answer names the job already started and the gateway spawns nothing. Only
//!   a submit the gateway never answered is repeated; one it refused is not.
//! - **Nothing is abandoned running.** A cancel, a deadline, a submit whose
//!   outcome stays unknown, or a poll that cannot be answered sends the
//!   gateway's cancel for the handle before this returns, so the GPU stops
//!   working on a page nobody will collect. A cancel before the submit sends
//!   nothing.
//! - **Outcomes keep their meaning for the journal.** A job the gateway says
//!   failed is [`HttpTransportError::JobFailed`] with its typed code, a cancel
//!   the gateway confirmed is [`HttpTransportError::JobCancelled`], and every
//!   case where the remote state stays unknown (unconfirmed cancel, deadline,
//!   transport) keeps a transport error, which callers journal as unknown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::inference::http::HttpTransportError;

/// Modal's web request limit: past it the answer is a 303 redirect.
pub const PROVIDER_REDIRECT: Duration = Duration::from_secs(150);
/// A submit: the upload of a page (up to 24 MB) and the gateway's spawn.
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(90);
/// A status read: a completed page's answer carries up to 24 MB.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(90);
/// A cancel: one small answer.
pub const CANCEL_TIMEOUT: Duration = Duration::from_secs(20);

const _: () = assert!(SUBMIT_TIMEOUT.as_secs() + 30 <= PROVIDER_REDIRECT.as_secs());
const _: () = assert!(STATUS_TIMEOUT.as_secs() + 30 <= PROVIDER_REDIRECT.as_secs());
const _: () = assert!(CANCEL_TIMEOUT.as_secs() + 30 <= PROVIDER_REDIRECT.as_secs());

/// How one kind of job is waited on.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// The first wait after the submit; each wait then doubles up to `max_poll`.
    pub first_poll: Duration,
    pub max_poll: Duration,
    /// The longest a job may take from its submit, queueing and a cold start
    /// included. Past it the job is cancelled and reported as not finished.
    pub deadline: Duration,
    /// Submits sent for one job when the gateway does not answer.
    pub submit_attempts: u32,
    /// Status reads in a row that may go unanswered before the job is given up.
    pub poll_failures: u32,
}

/// A detection tile: well under a second warm, a cold start of the analysis GPU
/// (its startup timeout is 600 s with page denoise on) when it is not.
pub const ANALYSIS_SCHEDULE: Schedule = Schedule {
    first_poll: Duration::from_millis(500),
    max_poll: Duration::from_secs(2),
    deadline: Duration::from_secs(900),
    submit_attempts: 3,
    poll_failures: 5,
};

/// An analysis batch: up to 16 requests, about 3 s of GPU work for a whole
/// page warm and a cold start when not. Read after 0.5 s and then every
/// second, not on the tile's backoff to 2 s, which on 2026-10-01 saw a job
/// done at 1.5 s only at 3.5 s.
pub const ANALYSIS_BATCH_SCHEDULE: Schedule = Schedule {
    first_poll: Duration::from_millis(500),
    max_poll: Duration::from_secs(1),
    deadline: Duration::from_secs(900),
    submit_attempts: 3,
    poll_failures: 5,
};

/// A denoise page: a cold start plus up to the analysis GPU's 600 s call
/// timeout (`deploy/cloud/modal/app.py` `ANALYSIS_TIMEOUT_SECONDS`).
pub const DENOISE_SCHEDULE: Schedule = Schedule {
    first_poll: Duration::from_secs(1),
    max_poll: Duration::from_secs(3),
    deadline: Duration::from_secs(1500),
    submit_attempts: 3,
    poll_failures: 5,
};

/// What one status read says.
#[derive(Debug, Clone, PartialEq)]
pub enum Polled<R> {
    /// Pending or running: ask again.
    Waiting,
    Completed(R),
    /// The gateway's typed failure code.
    Failed { code: String },
    Cancelled,
}

/// One job on the wire. The handle and the submit body are fixed for its life.
pub trait JobTransport {
    type Output;
    /// Send the submit. `Ok` once the gateway accepted this job's handle.
    fn submit(&self, timeout: Duration) -> Result<(), HttpTransportError>;
    fn status(&self, timeout: Duration) -> Result<Polled<Self::Output>, HttpTransportError>;
    /// Ask the gateway to cancel. `Ok(true)` when it says the job is over.
    fn cancel(&self, timeout: Duration) -> Result<bool, HttpTransportError>;
}

pub trait Clock {
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

pub struct SystemClock(Instant);

impl SystemClock {
    pub fn new() -> Self { Self(Instant::now()) }
}

impl Default for SystemClock {
    fn default() -> Self { Self::new() }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration { self.0.elapsed() }
    fn sleep(&self, duration: Duration) { std::thread::sleep(duration) }
}

/// A cancel is seen within this long while waiting between reads.
const CANCEL_CHECK: Duration = Duration::from_millis(250);

/// A submit the gateway did not answer: it may or may not have been accepted.
fn unanswered(error: &HttpTransportError) -> bool {
    matches!(error, HttpTransportError::ConnectionError | HttpTransportError::Timeout)
}

/// A status read worth repeating: unanswered, or the gateway's store hiccuped.
fn transient(error: &HttpTransportError) -> bool {
    unanswered(error) || matches!(error, HttpTransportError::UnexpectedStatus { status: 502..=504 })
}

/// Wait `duration`, in slices, and say whether a cancel came meanwhile.
fn wait(clock: &dyn Clock, duration: Duration, cancel: &AtomicBool) -> bool {
    let mut left = duration;
    while !left.is_zero() {
        if cancel.load(Ordering::SeqCst) { return true; }
        let step = left.min(CANCEL_CHECK);
        clock.sleep(step);
        left -= step;
    }
    cancel.load(Ordering::SeqCst)
}

/// Cancel on the gateway and name the outcome: confirmed, or still unknown.
fn stop<T: JobTransport + ?Sized>(transport: &T) -> HttpTransportError {
    match transport.cancel(CANCEL_TIMEOUT) {
        Ok(true) => HttpTransportError::JobCancelled,
        _ => HttpTransportError::JobCancelUnconfirmed,
    }
}

/// Submit one job and poll it to its end. See the module docs.
pub fn run_job<T: JobTransport + ?Sized>(
    transport: &T,
    schedule: &Schedule,
    cancel: &AtomicBool,
    clock: &dyn Clock,
) -> Result<T::Output, HttpTransportError> {
    if cancel.load(Ordering::SeqCst) { return Err(HttpTransportError::JobCancelled); }
    let mut attempt = 0;
    loop {
        attempt += 1;
        match transport.submit(SUBMIT_TIMEOUT) {
            Ok(()) => break,
            // The same body again names the same job, so this never pays twice.
            Err(error) if unanswered(&error) && attempt < schedule.submit_attempts => {
                if wait(clock, schedule.first_poll, cancel) { return Err(stop(transport)); }
            }
            Err(error) if unanswered(&error) => {
                // Perhaps accepted: make sure it does not run for nobody.
                let _ = transport.cancel(CANCEL_TIMEOUT);
                return Err(error);
            }
            // The gateway answered: nothing was queued.
            Err(error) => return Err(error),
        }
    }
    let started = clock.now();
    let mut interval = schedule.first_poll;
    let mut failures = 0;
    loop {
        if wait(clock, interval, cancel) { return Err(stop(transport)); }
        if clock.now().saturating_sub(started) >= schedule.deadline {
            let _ = transport.cancel(CANCEL_TIMEOUT);
            return Err(HttpTransportError::JobDeadline);
        }
        match transport.status(STATUS_TIMEOUT) {
            Ok(Polled::Waiting) => {
                failures = 0;
                interval = (interval * 2).min(schedule.max_poll);
            }
            Ok(Polled::Completed(output)) => return Ok(output),
            Ok(Polled::Failed { code }) => return Err(HttpTransportError::JobFailed { code }),
            Ok(Polled::Cancelled) => return Err(HttpTransportError::JobCancelled),
            Err(error) if transient(&error) && failures + 1 < schedule.poll_failures => failures += 1,
            Err(error) => {
                let _ = transport.cancel(CANCEL_TIMEOUT);
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    /// Simulated time: sleeping and every request advance it.
    struct SimClock(Cell<Duration>);

    impl Clock for SimClock {
        fn now(&self) -> Duration { self.0.get() }
        fn sleep(&self, duration: Duration) { self.0.set(self.0.get() + duration) }
    }

    /// A gateway whose job completes at `done_at` of simulated time. Every
    /// request takes `latency` and records the timeout it was sent with; a
    /// request whose timeout is past the provider's redirect fails the test.
    struct FakeJob<'a> {
        clock: &'a SimClock,
        latency: Duration,
        done_at: Duration,
        submits: RefCell<VecDeque<Result<(), HttpTransportError>>>,
        statuses: RefCell<VecDeque<Result<Polled<u32>, HttpTransportError>>>,
        /// `None`: the cancel goes unanswered.
        cancel_answer: Option<bool>,
        timeouts: RefCell<Vec<Duration>>,
        sent: Cell<(u32, u32, u32)>,
        cancel_at: Option<(&'a AtomicBool, Duration)>,
    }

    impl<'a> FakeJob<'a> {
        fn new(clock: &'a SimClock, done_at: Duration) -> Self {
            Self {
                clock, latency: Duration::from_millis(200), done_at,
                submits: RefCell::new(VecDeque::new()), statuses: RefCell::new(VecDeque::new()),
                cancel_answer: Some(true), timeouts: RefCell::new(Vec::new()), sent: Cell::new((0, 0, 0)),
                cancel_at: None,
            }
        }

        fn request(&self, timeout: Duration) {
            assert!(timeout < PROVIDER_REDIRECT, "a request may run into the provider's redirect");
            self.timeouts.borrow_mut().push(timeout);
            self.clock.sleep(self.latency);
            if let Some((flag, at)) = self.cancel_at {
                if self.clock.now() >= at { flag.store(true, Ordering::SeqCst); }
            }
        }
    }

    impl JobTransport for FakeJob<'_> {
        type Output = u32;
        fn submit(&self, timeout: Duration) -> Result<(), HttpTransportError> {
            self.request(timeout);
            let (s, p, c) = self.sent.get();
            self.sent.set((s + 1, p, c));
            self.submits.borrow_mut().pop_front().unwrap_or(Ok(()))
        }
        fn status(&self, timeout: Duration) -> Result<Polled<u32>, HttpTransportError> {
            self.request(timeout);
            let (s, p, c) = self.sent.get();
            self.sent.set((s, p + 1, c));
            if let Some(answer) = self.statuses.borrow_mut().pop_front() { return answer; }
            Ok(if self.clock.now() >= self.done_at { Polled::Completed(7) } else { Polled::Waiting })
        }
        fn cancel(&self, timeout: Duration) -> Result<bool, HttpTransportError> {
            self.request(timeout);
            let (s, p, c) = self.sent.get();
            self.sent.set((s, p, c + 1));
            self.cancel_answer.ok_or(HttpTransportError::ConnectionError)
        }
    }

    fn clock() -> SimClock { SimClock(Cell::new(Duration::ZERO)) }

    #[test]
    fn a_job_far_past_the_redirect_completes_without_a_long_request() {
        let clock = clock();
        let job = FakeJob::new(&clock, Duration::from_secs(400));
        let result = run_job(&job, &DENOISE_SCHEDULE, &AtomicBool::new(false), &clock);
        assert_eq!(result, Ok(7));
        assert!(clock.now() > PROVIDER_REDIRECT + Duration::from_secs(200), "{:?}", clock.now());
        let timeouts = job.timeouts.borrow();
        assert!(timeouts.iter().all(|timeout| *timeout <= STATUS_TIMEOUT.max(SUBMIT_TIMEOUT)));
        let (submits, polls, cancels) = job.sent.get();
        assert_eq!((submits, cancels), (1, 0));
        // Backed off to the longest interval: about one read per 3 s, not a spin.
        assert!((100..200).contains(&polls), "{polls} reads for 400 s");
    }

    #[test]
    fn an_unanswered_submit_is_repeated_and_an_answered_one_is_not() {
        let clock = clock();
        let job = FakeJob::new(&clock, Duration::from_secs(5));
        job.submits.borrow_mut().extend([Err(HttpTransportError::Timeout), Err(HttpTransportError::ConnectionError)]);
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock), Ok(7));
        assert_eq!(job.sent.get().0, 3);

        let job = FakeJob::new(&clock, Duration::ZERO);
        job.submits.borrow_mut().push_back(Err(HttpTransportError::UnexpectedStatus { status: 400 }));
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock),
            Err(HttpTransportError::UnexpectedStatus { status: 400 }));
        assert_eq!(job.sent.get(), (1, 0, 0), "a refused submit is neither repeated nor cancelled");
    }

    #[test]
    fn a_submit_never_answered_is_cancelled_before_giving_up() {
        let clock = clock();
        let job = FakeJob::new(&clock, Duration::ZERO);
        job.submits.borrow_mut().extend([Err(HttpTransportError::Timeout), Err(HttpTransportError::Timeout),
            Err(HttpTransportError::Timeout)]);
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock), Err(HttpTransportError::Timeout));
        assert_eq!(job.sent.get(), (3, 0, 1));
    }

    #[test]
    fn a_cancel_stops_polling_and_cancels_the_job() {
        let clock = clock();
        let flag = AtomicBool::new(false);
        let mut job = FakeJob::new(&clock, Duration::from_secs(400));
        job.cancel_at = Some((&flag, Duration::from_secs(30)));
        assert_eq!(run_job(&job, &DENOISE_SCHEDULE, &flag, &clock), Err(HttpTransportError::JobCancelled));
        let (_, polls, cancels) = job.sent.get();
        assert_eq!(cancels, 1);
        assert!(polls < 20, "{polls}");
        assert!(clock.now() < Duration::from_secs(40), "the cancel was not seen promptly: {:?}", clock.now());

        let clock = self::clock();
        let flag = AtomicBool::new(false);
        let mut job = FakeJob::new(&clock, Duration::from_secs(400));
        job.cancel_at = Some((&flag, Duration::from_secs(30)));
        job.cancel_answer = None;
        assert_eq!(run_job(&job, &DENOISE_SCHEDULE, &flag, &clock), Err(HttpTransportError::JobCancelUnconfirmed));
    }

    #[test]
    fn a_cancel_before_the_submit_sends_nothing() {
        let clock = clock();
        let job = FakeJob::new(&clock, Duration::ZERO);
        assert_eq!(run_job(&job, &DENOISE_SCHEDULE, &AtomicBool::new(true), &clock), Err(HttpTransportError::JobCancelled));
        assert_eq!(job.sent.get(), (0, 0, 0));
    }

    #[test]
    fn a_job_past_its_deadline_is_cancelled() {
        let clock = clock();
        let job = FakeJob::new(&clock, Duration::from_secs(100_000));
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock), Err(HttpTransportError::JobDeadline));
        assert_eq!(job.sent.get().2, 1);
        assert!(clock.now() >= ANALYSIS_SCHEDULE.deadline);
        assert!(clock.now() < ANALYSIS_SCHEDULE.deadline + Duration::from_secs(10));
    }

    #[test]
    fn a_few_unanswered_reads_are_survived_and_many_are_not() {
        let clock = clock();
        let job = FakeJob::new(&clock, Duration::from_secs(60));
        job.statuses.borrow_mut().extend([Err(HttpTransportError::Timeout), Ok(Polled::Waiting),
            Err(HttpTransportError::UnexpectedStatus { status: 503 }), Err(HttpTransportError::ConnectionError),
            Err(HttpTransportError::Timeout), Err(HttpTransportError::Timeout)]);
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock), Ok(7));

        let job = FakeJob::new(&clock, Duration::from_secs(100_000));
        job.statuses.borrow_mut().extend((0..5).map(|_| Err(HttpTransportError::ConnectionError)));
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock), Err(HttpTransportError::ConnectionError));
        assert_eq!(job.sent.get(), (1, 5, 1), "given up after five, and cancelled");

        let job = FakeJob::new(&clock, Duration::from_secs(100_000));
        job.statuses.borrow_mut().push_back(Err(HttpTransportError::UnexpectedStatus { status: 404 }));
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock),
            Err(HttpTransportError::UnexpectedStatus { status: 404 }));
        assert_eq!(job.sent.get(), (1, 1, 1), "an unknown job is not asked about again");
    }

    #[test]
    fn a_failed_or_cancelled_job_keeps_its_meaning() {
        let clock = clock();
        let job = FakeJob::new(&clock, Duration::from_secs(100_000));
        job.statuses.borrow_mut().push_back(Ok(Polled::Failed { code: "worker_timeout".into() }));
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock),
            Err(HttpTransportError::JobFailed { code: "worker_timeout".into() }));
        let job = FakeJob::new(&clock, Duration::from_secs(100_000));
        job.statuses.borrow_mut().push_back(Ok(Polled::Cancelled));
        assert_eq!(run_job(&job, &ANALYSIS_SCHEDULE, &AtomicBool::new(false), &clock), Err(HttpTransportError::JobCancelled));
        assert_eq!(job.sent.get().2, 0);
    }
}
