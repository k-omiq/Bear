//! Talking to the sidecar, and owning the process that answers.
//!
//! Two values, because two things are being owned and only one of them is
//! always ours. [`Client`] is the protocol against an address: it knows the
//! token, the contract, the budget and the deadline, and it has no idea whether
//! anything is a child of this process. [`Sidecar`] is a [`Client`] with a
//! [`std::process::Child`] behind it, spawned here and killed on drop.
//!
//! The split is not tidiness. It is what makes rung 3a testable on a machine
//! with no Python at all - [`crate::engines::flux`]'s tests stand a
//! [`std::net::TcpListener`] up in a thread, speak the protocol back, and drive
//! a real region through a real composite. And it is what lets a developer run
//! the Python half by hand: [`Sidecar::attach`] takes an address and a token
//! and spawns nothing, which is the only way to debug a sidecar under a
//! debugger of its own.
//!
//! ## What is enforced here rather than asked for
//!
//! Rule 9 is a contract on a
//! process we did not write, so most of it can only be *checked* from this
//! side. Three things are genuinely enforced here:
//!
//! - **The open fails when a required guard was not applied.** The sidecar
//!   echoes what it did; [`super::backend::Contract::satisfied_by`] compares
//!   that against the backend's own list; a short echo shuts the child down. A
//!   sidecar that quietly skipped `mx.set_cache_limit` - exactly this failure -
//!   does not get to run a region.
//! - **The peak is checked after every region, against the budget.** This is
//!   the whole of the bound for a backend whose allocator has no setter
//!   ([`super::backend::Cap::Parent`]) and the second line of defence for one
//!   that does. Over the cap, the child is shut down and the region declines to
//!   rung 2 - rule 9's third guard, one region later than an allocator would
//!   have managed it, and still into the ladder rather than into the
//!   compressor.
//! - **One region in flight.** [`Client::render`] takes `&mut self`, so the
//!   type system holds rule 6 on this side; the protocol holds it on the other
//!   by answering `409 busy` rather than queueing.
//!
//! ## The floor between regions is measured, not assumed
//!
//! Rule 9's last guard ends *"if it ratchets instead, it is not honouring this
//! document and the rung is not shippable. That is a measurement, not an
//! assumption."* [`Floor`] is the seam that measurement goes through: every
//! reply's memory block is recorded, the first and last resident readings are
//! kept, and [`Floor::ratchet`] is the difference. Nothing here fails a run on
//! a ratchet, because what "flat" means in bytes is exactly the constant this
//! file will not invent. What is built is the instrument.

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::engines::model::{Decline, Error};

use super::backend::{Backend, Contract};
use super::hardware::Budget;
use super::http;
use super::wire::{
    self, ErrorBody, Health, MemoryReport, OpenReply, OpenRequest, RenderReply, RenderRequest,
};
use super::Install;

/// How long one `GET /v1/health` may take. Short: it is a liveness probe, and a
/// health endpoint that needs longer than this is a sidecar doing work on a
/// thread it should not be doing it on.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for a freshly spawned child to answer at all.
///
/// Thirty seconds, and the reason it is that long rather than two is the
/// reference implementation's: a cold first run pays a dynamic-library load and
/// the operating system's first-launch verification on top of an interpreter
/// start. The wait exits **early** on a child that has already exited, which is
/// what makes the number affordable - a sidecar that failed to import
/// something is reported in a second, not in thirty.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// The interval between health polls during startup.
pub const STARTUP_POLL: Duration = Duration::from_millis(250);

/// How long `POST /v1/open` may take. Ten minutes: it reads several gigabytes
/// of weights off disk and builds a model, and on a first run it may be doing
/// that from a cold page cache.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(600);

/// How long one region may take.
///
/// §4a budgets "ten to sixty seconds a
/// region"; five minutes is five times the top of that and still a bound. A
/// deadline is not a performance target - it is the difference between a hung
/// sidecar being a lost region and a hung sidecar being a lost afternoon.
pub const RENDER_TIMEOUT: Duration = Duration::from_secs(300);

/* ------------------------------------------------------------------ */
/* The floor                                                           */
/* ------------------------------------------------------------------ */

/// What the sidecar's memory did across a page, as rule 9's fifth guard asks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Floor {
    /// How many regions have been rendered through this sidecar.
    pub regions: u32,
    /// Resident bytes after the first region, where the sidecar could say.
    pub first_rss: Option<u64>,
    /// Resident bytes after the most recent one.
    pub last_rss: Option<u64>,
    /// The high-water mark the sidecar has reported, ever.
    pub peak: u64,
    /// What the backend's cache was last holding.
    pub cache: Option<u64>,
}

impl Floor {
    fn observe(&mut self, report: &MemoryReport) {
        self.peak = self.peak.max(report.peak_rss_bytes);
        self.cache = report.cache_bytes;
        if let Some(rss) = report.rss_bytes {
            self.first_rss.get_or_insert(rss);
            self.last_rss = Some(rss);
        }
    }

    /// How far the resident floor has moved between the first region and the
    /// last, in bytes. `None` where the sidecar reports no resident figure.
    ///
    /// Positive is a ratchet, which is what rule 9 forbids. Nothing here judges
    /// it: **the threshold is still to be measured**, and a constant invented
    /// here would be a criterion fitted to the first sidecar that ran rather
    /// than to the rule.
    pub fn ratchet(&self) -> Option<i64> {
        Some(self.last_rss? as i64 - self.first_rss? as i64)
    }
}

/* ------------------------------------------------------------------ */
/* The protocol, against an address                                    */
/* ------------------------------------------------------------------ */

pub struct Client {
    addr: SocketAddr,
    token: String,
    contract: &'static Contract,
    budget: Budget,
    render_timeout: Duration,
    floor: Floor,
    /// Set when the peak passed the budget. Latched for [`super::Ladder`]'s
    /// reason: a sidecar that has already been over the cap once is not one to
    /// hand another region to, and the region after it would be a second
    /// chance to take the machine.
    exhausted: bool,
    opened: bool,
}

impl Client {
    pub fn new(addr: SocketAddr, token: String, backend: Backend, budget: Budget) -> Client {
        Client {
            addr,
            token,
            contract: backend.contract(),
            budget,
            render_timeout: RENDER_TIMEOUT,
            floor: Floor::default(),
            exhausted: false,
            opened: false,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn budget(&self) -> Budget {
        self.budget
    }

    pub fn contract(&self) -> &'static Contract {
        self.contract
    }

    pub fn floor(&self) -> Floor {
        self.floor
    }

    pub fn is_open(&self) -> bool {
        self.opened
    }

    /// Whether the peak has already passed the budget. See `exhausted`.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Override the per-region deadline. For the tests, and for a settings
    /// panel on a machine where sixty seconds is optimistic.
    pub fn with_render_timeout(mut self, timeout: Duration) -> Client {
        self.render_timeout = timeout;
        self
    }

    /// `GET /v1/health`. No token - a liveness probe that needed a secret would
    /// be a liveness probe that cannot distinguish "not up yet" from "wrong
    /// secret", and those have different remedies.
    pub fn health(&self) -> Result<Health, Error> {
        let reply = self.send("GET", "/v1/health", None, HEALTH_TIMEOUT)?;
        let health: Health = parse_body(&reply)?;
        if health.protocol != wire::PROTOCOL {
            return Err(Error::Run(format!(
                "the sidecar speaks protocol {} and this build speaks {}",
                health.protocol,
                wire::PROTOCOL
            )));
        }
        Ok(health)
    }

    /// `POST /v1/open`. Load the model, hand over the limits, and **refuse a
    /// sidecar that did not apply the guards its backend requires**.
    pub fn open(&mut self, model: &str) -> Result<OpenReply, Error> {
        let request = OpenRequest {
            backend: self.contract.id.to_owned(),
            model: model.to_owned(),
            limits: self.budget.limits(),
            require: self.contract.required.iter().map(|g| (*g).to_owned()).collect(),
        };
        let body = serde_json::to_vec(&request)
            .map_err(|e| Error::Run(format!("the open request would not serialise: {e}")))?;
        let reply = self.send("POST", "/v1/open", Some(&body), OPEN_TIMEOUT)?;
        let opened: OpenReply = parse_body(&reply)?;

        let missing = self.contract.missing_from(&opened.applied);
        if !missing.is_empty() {
            // Named, not counted. These guards were individually defensible
            // and collectively absent, and a log line saying "the open failed"
            // would reproduce exactly the condition that made that hard to see.
            return Err(Error::Run(format!(
                "the {} sidecar did not apply {} on the {} allocator",
                self.contract.id,
                missing.join(", "),
                self.contract.allocator
            )));
        }
        if let Some(memory) = &opened.memory {
            self.floor.observe(memory);
        }
        self.opened = true;
        Ok(opened)
    }

    /// `POST /v1/render`. One region, and the whole of rule 9's enforcement
    /// from this side.
    pub fn render(&mut self, request: &RenderRequest) -> Result<RenderReply, Error> {
        if self.exhausted {
            return Err(Decline::SidecarMemory.into());
        }
        let body = serde_json::to_vec(request)
            .map_err(|e| Error::Run(format!("the render request would not serialise: {e}")))?;
        let reply = self.send("POST", "/v1/render", Some(&body), self.render_timeout)?;
        let rendered: RenderReply = parse_body(&reply)?;

        // The geometry, exactly. This rung's model regenerates the whole crop,
        // so a reply of another size would mean resampling before compositing
        // - and §5.2 step 4 permits a resample only as a last resort with a
        // named filter, which is not something to do by accident.
        let (want, got) = (&request.image, &rendered.image);
        if (want.width, want.height, want.encoding) != (got.width, got.height, got.encoding) {
            return Err(Error::Run(format!(
                "asked for {}×{} {:?} and got {}×{} {:?}",
                want.width, want.height, want.encoding, got.width, got.height, got.encoding
            )));
        }

        self.floor.observe(&rendered.memory);
        self.floor.regions += 1;

        // **The parent's own cap.** Checked after the region rather than
        // before, because it is the region that spends the memory - and
        // declining the *next* one is what rule 9's "fail fast, into the
        // ladder" costs when the allocator underneath has no setter to raise
        // from.
        if rendered.memory.peak_rss_bytes > self.budget.total {
            self.exhausted = true;
            return Err(Decline::SidecarMemory.into());
        }
        Ok(rendered)
    }

    /// `POST /v1/release`. Give the model back without ending the process -
    /// "restarted rather than
    /// reconfigured" applies to a *model change*, and this is the cheaper case:
    /// the same model, no longer needed.
    pub fn release(&mut self) -> Result<(), Error> {
        let reply = self.send("POST", "/v1/release", None, HEALTH_TIMEOUT)?;
        self.opened = false;
        if let Ok(report) = serde_json::from_slice::<MemoryReport>(&reply) {
            self.floor.observe(&report);
        }
        Ok(())
    }

    /// One request, with the taxonomy applied to whatever comes back.
    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        let token = (path != "/v1/health").then_some(self.token.as_str());
        let reply = http::request(self.addr, method, path, token, body, timeout)
            .map_err(|e| Error::Run(format!("the sidecar at {} did not answer: {e}", self.addr)))?;
        if (200..300).contains(&reply.status) {
            return Ok(reply.body);
        }
        Err(self.failure(reply.status, &reply.body))
    }

    /// A non-2xx reply, as either a decline or a fault.
    fn failure(&self, status: u16, body: &[u8]) -> Error {
        let Ok(parsed) = serde_json::from_slice::<ErrorBody>(body) else {
            // A status with no taxonomy behind it is a fault whatever it is:
            // guessing a decline from a bare status code would route a region
            // down the ladder on the strength of a number a proxy could have
            // written.
            return Error::Run(format!(
                "the sidecar answered {status} with a body this does not understand"
            ));
        };
        let detail = parsed.error.detail.unwrap_or_default();
        if parsed.error.kind.declines() {
            return match parsed.error.kind {
                wire::ErrorKind::OutOfMemory => Decline::SidecarMemory.into(),
                wire::ErrorKind::UnboundedBackend => Decline::SidecarUnbounded.into(),
                // `WeightsMissing`, and the wildcard is unreachable -
                // [`wire::ErrorKind::declines`] names exactly three.
                _ => Decline::SidecarWeightsMissing.into(),
            };
        }
        Error::Run(format!("the sidecar answered {status} {:?}: {detail}", parsed.error.kind))
    }
}

fn parse_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(body).map_err(|e| {
        Error::Run(format!("the sidecar's reply is not the shape this protocol declares: {e}"))
    })
}

/* ------------------------------------------------------------------ */
/* The child                                                           */
/* ------------------------------------------------------------------ */

/// A [`Client`] and the process it talks to.
pub struct Sidecar {
    client: Client,
    /// `None` when this process did not spawn it - [`Sidecar::attach`].
    child: Option<Child>,
    /// The loaded-models row for the child, and `None` for exactly the same
    /// reason `child` is: a sidecar this process did not start is not one it
    /// can hand back, and a row with a close button that cannot close is worse
    /// than no row ([`crate::registry`]).
    lease: Option<crate::registry::Lease>,
}

impl Sidecar {
    /// Spawn the sidecar, wait for it to answer, and hand back a client.
    ///
    /// The five mechanisms here are the reference implementation's, and they
    /// are copied because each one closes a failure this rung would otherwise
    /// hit: an algorithm reference by copy, never by import.
    ///
    /// - **An ephemeral port**, from binding `:0` and reading the number back.
    ///   A fixed port collides with a stale instance, and the collision looks
    ///   exactly like a sidecar that came up and then misbehaved. There is a
    ///   window between releasing the listener and the child binding it; losing
    ///   that race costs a failed startup that is reported, which is strictly
    ///   better than the silent clash a fixed number gives.
    /// - **A 24-byte secret from the operating system's own generator**, in a
    ///   header. The port is loopback and every other process on the machine
    ///   can reach loopback.
    /// - **`/v1/health` polled at 250 ms**, so a fast start is fast.
    /// - **`try_wait` on every poll**, so a child that died on an import error
    ///   ends the wait immediately instead of at the timeout.
    /// - **The parent's pid in the environment**, for the watchdog the child
    ///   runs ("it **dies with us** -
    ///   a parent watchdog, so a crash cannot orphan a multi-gigabyte process
    ///   holding a port").
    pub fn spawn(
        install: &Install,
        backend: Backend,
        model: &str,
        budget: Budget,
    ) -> Result<Sidecar, Error> {
        let token = token()?;
        let port = free_port()?;
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();

        let mut command = Command::new(&install.python);
        let weights_dir = std::env::var_os("MC_SIDECAR_WEIGHTS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| match backend {
                Backend::Mflux => install.root.join("weights").join("flux2-klein-4b-mflux-q4"),
                _ => install.root.join("weights"),
            });
        command
            .arg("-m")
            .arg(super::MODULE)
            .current_dir(&install.root)
            .env("MC_SIDECAR_HOST", "127.0.0.1")
            .env("MC_SIDECAR_PORT", port.to_string())
            .env("MC_SIDECAR_TOKEN", &token)
            .env("MC_SIDECAR_PARENT_PID", std::process::id().to_string())
            .env("MC_SIDECAR_BACKEND", backend.id())
            .env("MC_SIDECAR_MODEL", model)
            .env("MC_SIDECAR_WEIGHTS_DIR", weights_dir)
            .env("MC_SIDECAR_TOTAL_BYTES", budget.total.to_string())
            .env("MC_SIDECAR_CACHE_BYTES", budget.cache.to_string())
            // Piped rather than inherited: a windowed release build has no
            // console, so an import error on the child's stderr would vanish.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|e| {
            Error::Run(format!("the sidecar at {} would not start: {e}", install.python.display()))
        })?;
        drain(child.stdout.take());
        drain(child.stderr.take());

        let client = Client::new(addr, token, backend, budget);
        // Registered **before** the health wait, not after it: a child that is
        // reading gigabytes off disk and has not answered yet is the moment the
        // tab most needs to say something is loading, and a row that only
        // appeared once the child was ready would be blank for exactly the
        // thirty seconds it matters. The size starts at zero - nothing has
        // reported one - and [`Sidecar::note_memory`] fills it in.
        let lease = crate::registry::register_named(
            crate::registry::Kind::Sidecar,
            crate::registry::Footprint::reported(0),
            crate::registry::Device::sidecar(),
            Some(model.to_owned()),
        );
        let mut sidecar = Sidecar { client, child: Some(child), lease: Some(lease) };
        sidecar.wait_for_health()?;
        Ok(sidecar)
    }

    /// A client for a sidecar somebody else is running, at a known address.
    ///
    /// This process kills nothing it did not start, which is the whole
    /// difference: a hand-run sidecar under a debugger survives the application
    /// exiting, and a test's fake sidecar is the test's to shut down.
    pub fn attach(addr: SocketAddr, token: String, backend: Backend, budget: Budget) -> Sidecar {
        Sidecar { client: Client::new(addr, token, backend, budget), child: None, lease: None }
    }

    pub fn client(&mut self) -> &mut Client {
        &mut self.client
    }

    /// Put what the child last said about its own memory onto its
    /// loaded-models row.
    ///
    /// The sidecar is the only loaded thing that *reports* its resident set
    /// ([`crate::sidecar::wire::MemoryReport`]), so its row carries a number
    /// nothing in this process had to estimate - which is why
    /// [`crate::registry::Basis::Reported`] exists at all. Called after every
    /// exchange that carries a memory block.
    pub fn note_memory(&mut self) {
        let Some(lease) = self.lease.as_ref() else {
            return;
        };
        lease.touch();
        let floor = self.client.floor();
        let bytes = floor.last_rss.unwrap_or(floor.peak);
        if bytes > 0 {
            lease.resize(crate::registry::Footprint::reported(bytes));
        }
    }

    /// Whether the child should be shut down at the holder's next safe point:
    /// somebody pressed close on its row, or nothing has sent it a region for
    /// [`crate::registry::SIDECAR_IDLE_GRACE`].
    ///
    /// **A sidecar that has been over its cap is spent whatever the clock
    /// says.** [`Client::exhausted`] is latched for the reason stated on the
    /// field - a child that took more than the budget once does not get another
    /// region - so holding it between two edits would be holding gigabytes that
    /// can no longer do any work.
    ///
    /// A sidecar this process did not spawn ([`Sidecar::attach`]) has no lease
    /// and is never spent: it is not ours to shut down.
    pub fn spent(&self) -> bool {
        self.client.is_exhausted()
            || self.lease.as_ref().is_some_and(crate::registry::Lease::spent)
    }

    /// Poll until it answers, the child exits, or the wait runs out.
    fn wait_for_health(&mut self) -> Result<Health, Error> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let mut last;
        loop {
            if let Some(child) = self.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return Err(Error::Run(format!(
                            "the sidecar exited during startup ({status})"
                        )));
                    }
                    Ok(None) => {}
                    // Cannot tell. Treated as alive, because the health poll is
                    // the authority and this is only an early exit from it.
                    Err(_) => {}
                }
            }
            match self.client.health() {
                Ok(health) => return Ok(health),
                Err(error) => last = error.to_string(),
            }
            if Instant::now() >= deadline {
                return Err(Error::Run(format!("the sidecar never became healthy: {last}")));
            }
            std::thread::sleep(STARTUP_POLL);
        }
    }

    /// Ask it to stop, then make sure.
    ///
    /// `POST /v1/shutdown` first so the child can drop its weights and exit on
    /// its own; the kill is what makes the promise unconditional. Best effort
    /// throughout - a shutdown path that can fail is a shutdown path that
    /// leaks a multi-gigabyte process.
    pub fn shutdown(&mut self) {
        let _ = http::request(
            self.client.addr,
            "POST",
            "/v1/shutdown",
            Some(&self.client.token),
            None,
            HEALTH_TIMEOUT,
        );
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The row goes with the process, and it goes here rather than only in
        // `Drop` because `shutdown` is public: a caller that stops the child
        // and keeps the value would otherwise leave the tab advertising
        // gigabytes that are not resident anywhere.
        self.lease = None;
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Read a child's pipe on a thread of its own so a full buffer cannot block it.
///
/// The lines are dropped rather than logged: `cleaner-core` has no logger and
/// is not the layer that should acquire one. What matters here is that the pipe
/// keeps draining - a child whose stderr buffer fills stops making progress,
/// and it stops during weight loading, which reads as a hang.
fn drain<R: std::io::Read + Send + 'static>(pipe: Option<R>) {
    if let Some(pipe) = pipe {
        std::thread::spawn(move || {
            for _ in BufReader::new(pipe).lines().map_while(Result::ok) {}
        });
    }
}

/// A free loopback port, from the operating system.
fn free_port() -> Result<u16, Error> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .map_err(|e| Error::Run(format!("no loopback port was available: {e}")))
}

/// A 24-byte secret, hex, from the operating system's generator.
///
/// **There is no fallback, and that is the decision.** The reference
/// implementation falls back to nanoseconds and a pid when `getrandom` fails,
/// which is defensible for a token gating an OCR endpoint and is not defensible
/// here: any process on the machine can reach loopback, launch time and pid are
/// both observable, and what sits behind this token is a request to run an
/// arbitrary prompt through a multi-gigabyte model with the user's page in it.
/// A machine that cannot produce 24 random bytes does not get a sidecar.
fn token() -> Result<String, Error> {
    read_random(24).map(|bytes| bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `n` bytes from the operating system's own generator, on every platform this
/// application ships to.
///
/// **This was `/dev/urandom` and nothing else**, which returned an error on
/// Windows and was therefore a *second*, independent block on the platforms
/// this was about - a rung that would
/// have stayed shut even once the memory probe answered. The remedy was framed
/// as
/// "`BCryptGenRandom`, or `getrandom` as a dependency - a decision about adding
/// one, not about cryptography", and this is that decision taken the second
/// way: `getrandom` is `BCryptGenRandom` on Windows, the `getrandom(2)` syscall
/// on Linux and `getentropy` on macOS, which is one call for three correct
/// answers instead of three `cfg` arms this build could only ever test one of.
///
/// It is a dependency the lockfile already carried several times over, so what
/// it adds to the build is a name in a manifest rather than a crate to compile.
///
/// **The token it produces is unchanged**: 24 bytes, 48 hex characters, and the
/// no-fallback rule above still stands - a failure here is an error and never a
/// weaker secret.
fn read_random(n: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::Run(format!("the system random source would not answer: {e}")))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_forty_eight_hex_characters_and_never_the_same_twice() {
        let (a, b) = (token().unwrap(), token().unwrap());
        assert_eq!(a.len(), 48);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn a_free_port_is_a_real_one() {
        assert!(free_port().unwrap() > 0);
    }

    /// The instrument rule 9's fifth guard asks for, and nothing more: it
    /// records, it does not judge. The threshold is still to be measured.
    #[test]
    fn the_floor_records_the_first_and_last_readings_and_the_high_water() {
        let mut floor = Floor::default();
        floor.observe(&MemoryReport {
            rss_bytes: Some(5 * 1024 * 1024),
            peak_rss_bytes: 9 * 1024 * 1024,
            cache_bytes: Some(0),
            ..Default::default()
        });
        floor.observe(&MemoryReport {
            rss_bytes: Some(7 * 1024 * 1024),
            peak_rss_bytes: 8 * 1024 * 1024,
            cache_bytes: Some(1024),
            ..Default::default()
        });
        assert_eq!(floor.first_rss, Some(5 * 1024 * 1024));
        assert_eq!(floor.last_rss, Some(7 * 1024 * 1024));
        // A high-water mark never comes back down, even when a later report is
        // lower.
        assert_eq!(floor.peak, 9 * 1024 * 1024);
        assert_eq!(floor.ratchet(), Some(2 * 1024 * 1024));
        assert_eq!(floor.cache, Some(1024));
    }

    #[test]
    fn a_floor_with_no_resident_readings_reports_no_ratchet_rather_than_none_of_one() {
        let mut floor = Floor::default();
        floor.observe(&MemoryReport {
            rss_bytes: None,
            peak_rss_bytes: 1,
            cache_bytes: None,
            ..Default::default()
        });
        assert_eq!(floor.ratchet(), None);
        assert_eq!(floor.peak, 1);
    }
}
