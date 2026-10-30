//! Rung 3a's out-of-process engine: finding it, gating it, spawning it,
//! speaking to it, and - the ordinary case - noticing that it is not there.
//!
//! §4a is the specification.
//! Rule 9 is the contract it has
//! to satisfy before it ships, and §6 is
//! the lifecycle. This module owns the half of all three that is Rust's. The
//! Python half lives in `sidecar/manga_cleaner_sidecar/` and was written
//! against "The wire" below, which is why that section is longer than the code
//! it describes. The two halves have never been driven against each other:
//! every test here speaks the protocol over a `TcpListener`, and the Python
//! self-test drives its own server, so the spawn path between them is the seam
//! neither exercises.
//!
//! ## Absence is the normal state
//!
//! The application ships no Python, so on
//! every machine that has not deliberately installed something, this rung is
//! simply not there. That is not an error, not a warning, and not a thing the
//! user dismisses: [`find`] answers `None`, [`availability`] answers
//! [`Availability::Absent`], and [`Availability::reason_key`] answers `None` for
//! that case alone - every other refusal has a sentence and this one has
//! nothing to say. A control that would fail is not shown; §4a's requirement is
//! that a user "is told why rather than shown a control that fails", and the
//! answer to *why* for a machine that never installed anything is that there is
//! nothing to tell.
//!
//! ## Two backends, and the platform picks a default
//!
//! [`Backend::Mflux`] is MLX and therefore Apple Silicon; [`Backend::Sdnq`] is
//! `torch` + `diffusers` and runs on CUDA, Intel XPU or Metal, which is every
//! platform this application builds for. So on a Mac there are genuinely two
//! choices, the `fluxBackend` setting is what picks between them, and `auto`
//! resolves to `mflux` on Apple Silicon and `sdnq` elsewhere
//! ([`Backend::from_setting`]). [`Backend::Sdcpp`] is in the table and renders
//! nowhere; nothing offers it.
//!
//! What that changed here is the *shape of the platform refusal*. It used to be
//! a fact about the machine - Windows and Linux had no backend at all - and it
//! is now a fact about a **choice**: see [`hardware::platform_decline_for`].
//!
//! ## What this rung is, in one paragraph
//!
//! A model with **no mask channel**, run on a crop, whose boundary **our
//! compositor enforces**. §4a's candidate table settles it: the one candidate
//! that takes a mask is `FLUX.1-Fill-dev` and its licence is unrecommendable,
//! `FLUX.2 [klein]` is Apache-2.0 and edits by reference image, and
//! `Z-Image-Turbo` cannot serve the rung at all. So the shape is §5.2's - the
//! cloud rung's - for the same reason the cloud rung has it: *"a mask sent as a
//! reference image is advisory"*. The model regenerates the whole crop, we
//! composite through the local mask plus its isolation ramp, and §6's decline
//! metric is what catches the failures.
//!
//! ## Never reachable by an automatic run
//!
//! Same ruling the cloud rung carries: `runClean` has no confirmation
//! protocol, and a batch job must not spend ten to sixty seconds a region on
//! a heavy local model without one. Nothing in
//! this module is wired into `run::ladder_from`, and
//! `run::rung_3a_is_not_reachable_from_an_automatic_run` is what keeps that true
//! after this file stops being read. The rung is reached per region,
//! deliberately, from Content-aware fill - a Phase 5 manual tool that does not
//! exist yet. The engine-side entry point is [`crate::engines::flux`] and it is
//! callable today.
//!
//! ---
//!
//! # The wire
//!
//! Everything a second implementation needs. Where this section and the types
//! in [`wire`] disagree, the types are right and this is a bug.
//!
//! ## Transport
//!
//! HTTP/1.1 over TCP on **`127.0.0.1` only**. The parent binds `:0`, reads the
//! assigned port, releases it and passes the number to the child in
//! `MC_SIDECAR_PORT`; the child binds that port and no other interface. Bodies
//! are `application/json`, UTF-8, framed by `Content-Length`. The client sends
//! `Connection: close` on every request and reads to end-of-stream, so a reply
//! without `Content-Length` is also readable. No TLS, no chunked encoding, no
//! keep-alive requirement, no websockets.
//!
//! **Authentication** is a shared secret in an `x-mc-token` header, 48 hex
//! characters from 24 bytes of the operating system's own generator, passed to
//! the child in `MC_SIDECAR_TOKEN`. It is required on **every** endpoint except
//! `GET /v1/health`, which is exempt so that "not up yet" and "wrong secret"
//! stay distinguishable. A request with a missing or wrong token is `401`.
//!
//! **The child dies with the parent.** `MC_SIDECAR_PARENT_PID` carries this
//! process's pid and the child runs a watchdog on it - on POSIX, a thread that
//! checks `getppid()` and `kill(pid, 0)` every couple of seconds and calls
//! `os._exit(0)` when the parent is gone. This is required: a hard crash here
//! must not orphan a multi-gigabyte process
//! holding a port.
//!
//! ## The environment the child is spawned with
//!
//! | variable | what it carries |
//! |---|---|
//! | `MC_SIDECAR_HOST` | always `127.0.0.1` |
//! | `MC_SIDECAR_PORT` | the port to bind |
//! | `MC_SIDECAR_TOKEN` | the 48-character secret |
//! | `MC_SIDECAR_PARENT_PID` | this process's pid, for the watchdog |
//! | `MC_SIDECAR_BACKEND` | `mflux`, `sdnq` or `sdcpp` - see [`backend`] |
//! | `MC_SIDECAR_MODEL` | the model id, e.g. `flux2-klein-4b` |
//! | `MC_SIDECAR_TOTAL_BYTES` | the resident ceiling, from the gate |
//! | `MC_SIDECAR_CACHE_BYTES` | the buffer cache's share of it |
//!
//! The last two are repeated in `POST /v1/open`'s body. They are in the
//! environment as well because a backend whose allocator must be bounded
//! **before** the runtime initialises cannot wait for an HTTP request to arrive -
//! which is the situation `PYTORCH_MPS_HIGH_WATERMARK_RATIO` exists for on
//! the torch path, and the shape is general even though that particular
//! variable is the wrong one here.
//!
//! The command line is `<python> -m manga_cleaner_sidecar`, run with the
//! install root as its working directory.
//!
//! ## `GET /v1/health`
//!
//! No token. Must answer while the model is loading and while a render is in
//! flight - it is the liveness probe the startup poll uses at 250 ms
//! intervals, and a sidecar that blocks it during a sixty-second render cannot
//! be told apart from one that has hung.
//!
//! ```json
//! {
//!   "protocol": 1,
//!   "sidecar_version": "0.1.0",
//!   "state": "idle",
//!   "backend": "mflux",
//!   "model": { "id": "flux2-klein-4b", "quantization": "int4",
//!              "source": "mflux-community/flux2-klein-4b-mflux-q4" },
//!   "weights_bytes": 4619705407,
//!   "working_set_bytes": 10737418240,
//!   "basis": "declared",
//!   "applied": ["mx.set_memory_limit", "mx.set_cache_limit"],
//!   "backends": ["mflux", "sdnq"],
//!   "memory": { "rss_bytes": 148000000, "peak_rss_bytes": 152000000,
//!               "cache_bytes": 0 }
//! }
//! ```
//!
//! **`backends` is which backends the sidecar's environment can *import***, and
//! it is the one question this side cannot answer for itself: an install is
//! found by looking for a `pyvenv.cfg` and an interpreter beside it, and since
//! [`Backend::Sdnq`] a virtual environment may legitimately hold `mflux`'s
//! dependencies, `sdnq`'s, or both. A backend absent from a non-empty list is
//! refused before `POST /v1/open` is sent, under
//! [`Decline::SidecarBackendMissing`] - whose remedy is a `pip install` and is
//! therefore a different sentence from [`Decline::SidecarPlatform`]. **An
//! omitted or empty list is not a refusal**: the field was added after the
//! protocol shipped, so silence means *this sidecar does not answer the
//! question* ([`reports_backend`]).
//!
//! `state` is one of `idle`, `loading`, `ready`, `busy`. `protocol` must be
//! exactly [`wire::PROTOCOL`]; anything else is refused rather than negotiated.
//!
//! **`weights_bytes` and `working_set_bytes` must both be answered before the
//! model is loaded**, from the selection in the environment, because the
//! hardware gate runs on them and the whole point of the gate is to refuse
//! before anything large is read. `weights_bytes` is what is on disk;
//! `working_set_bytes` is the process's peak: the resident weights, and everything
//! the parameter count does not predict - latents, reference tokens, the VAE's
//! peak, whatever the allocator keeps.
//! Omitting `working_set_bytes` means *unbounded*, and an unbounded backend is
//! refused ([`hardware`]). A `working_set_bytes` below `weights_bytes` is
//! refused too, as a field that was misunderstood rather than a machine that is
//! generous.
//!
//! ## `POST /v1/open`
//!
//! ```json
//! {
//!   "backend": "mflux",
//!   "model": "flux2-klein-4b",
//!   "limits": { "total_bytes": 14495514624, "cache_bytes": 1811939328 },
//!   "require": ["mx.set_memory_limit", "mx.set_cache_limit"]
//! }
//! ```
//!
//! Load the model and apply the limits. The reply echoes **what was actually
//! applied**:
//!
//! ```json
//! {
//!   "applied": ["mx.set_memory_limit", "mx.set_cache_limit"],
//!   "weights_bytes": 4619705407,
//!   "working_set_bytes": 10737418240,
//!   "basis": "declared",
//!   "memory": { "rss_bytes": 5100000000, "peak_rss_bytes": 5400000000,
//!               "cache_bytes": 0 }
//! }
//! ```
//!
//! **An echo missing anything from `require` fails the open and the child is
//! shut down.** This is the one piece of rule 9 that a caller can actually
//! verify from outside, and it exists because the four missing guards it
//! replaced were
//! all *present in the source* of the process that lacked them. What each name
//! means, and which backend it is real on, is [`backend`].
//!
//! ## `POST /v1/render`
//!
//! ```json
//! {
//!   "region": "page-07:r4",
//!   "image": { "width": 768, "height": 512, "channels": 3,
//!              "encoding": "rgb8", "data": "<base64>" },
//!   "hint":  { "width": 768, "height": 512, "channels": 1,
//!              "encoding": "gray8", "data": "<base64>" },
//!   "prompt": "remove all Japanese text, …",
//!   "steps": 4,
//!   "seed": 1,
//!   "guidance": 1.0,
//!   "deadline_ms": 300000
//! }
//! ```
//!
//! `image` is the crop at the page's **native resolution**, already extended
//! with real surrounding page pixels; the parent never asks for a rescale.
//! `hint` is 255 where the text is and 0 elsewhere, and it is **advisory** -
//! these models take no mask, and the sidecar must not composite against it.
//! Returning the raw page under the hint would defeat the rung; returning an
//! edited crop is the job.
//!
//! ```json
//! {
//!   "image": { "width": 768, "height": 512, "channels": 3,
//!              "encoding": "rgb8", "data": "<base64>" },
//!   "elapsed_ms": 18342,
//!   "memory": { "rss_bytes": 5210000000, "peak_rss_bytes": 8900000000,
//!               "cache_bytes": 1200000000, "instrument": "phys_footprint",
//!               "footprint_bytes": 5210000000, "peak_footprint_bytes": 8900000000,
//!               "resident_bytes": 2380000000, "peak_resident_bytes": 2880000000,
//!               "backend_peak_bytes": 6650000000 }
//! }
//! ```
//!
//! **The reply's geometry must equal the request's, exactly**, and a reply that
//! differs is a fault rather than something to resample: §5.2 step 4 permits a
//! resample only as a last resort and names the filter, which is not a decision
//! to make by accident inside a transport.
//!
//! `memory` is **required** on every render reply. `peak_rss_bytes` is checked
//! against `limits.total_bytes` after every region, and a peak over the budget
//! shuts the sidecar down and declines the region to rung 2. `rss_bytes` is what
//! rule 9's "return to that floor between regions" is measured with; it is
//! optional only because not every runtime can ask for it cheaply, and a
//! sidecar that can should.
//!
//! **Both are physical footprints and neither is a resident set size**, despite
//! the field names, which are historical. On macOS the model lives in
//! unified-memory buffers a resident reading does not count at all: the first
//! measurement of this rung read 2.89 GB for a render that cost 6.58 GB, and the
//! same 2.89 GB again for a render at four times the latent area that cost
//! 11.46 GB. Since `peak_rss_bytes` is the number the budget is enforced
//! against, it has to be the quantity the user's machine actually loses. The
//! `instrument` field names the quantity outright - `"phys_footprint"` normally,
//! `"ru_maxrss"` from a sidecar that could not read a Mach ledger - and
//! `footprint_bytes`, `peak_footprint_bytes`, `resident_bytes`,
//! `peak_resident_bytes` and `backend_peak_bytes` carry each instrument under
//! its own name so the disagreement between them stays auditable. Nothing yet
//! *rejects* a sidecar reporting `"ru_maxrss"`.
//!
//! One region at a time. A second concurrent `POST /v1/render` is answered
//! `409` and **never queued** - a queue inside the sidecar is a queue this
//! process cannot see the depth of, and rule 6's "one window in flight" is
//! stated over the whole machine.
//!
//! ## `POST /v1/release` and `POST /v1/shutdown`
//!
//! `release` drops the model and returns to `idle`, answering a bare
//! [`wire::MemoryReport`]. `shutdown` answers `202` and exits. Both are best
//! effort from this side: the parent kills the child afterwards regardless,
//! because a shutdown path that can fail is a shutdown path that leaks a
//! multi-gigabyte process.
//!
//! A **model change is a restart, not a `release` then an `open`**
//! ("restarted rather than
//! reconfigured … because a process that has loaded one set of weights has no
//! way to prove it has given them back").
//!
//! ## Failure
//!
//! Every non-2xx reply is `{"error": {"kind": …, "detail": …}}` with a kind
//! from [`wire::ErrorKind`]. `detail` is English for the log and is never shown
//! to a user - no English crosses the seam, and this text comes from a file
//! the user installed.
//!
//! | status | `kind` | this side |
//! |---|---|---|
//! | 401 | `unauthorized` | fault - the sidecar on that port is not ours |
//! | 409 | `busy` | fault - one in flight is this side's count to keep |
//! | 412 | `not_open` | fault |
//! | 422 | `bad_request` | fault - geometry, encoding or base64 |
//! | 500 | `internal` | fault |
//! | 501 | `unbounded_backend` | **decline** - rule 9's first guard, refused from the far side. `sdcpp` always; `sdnq` on a machine with no CUDA, XPU or Metal device, where neither of its guards can be applied |
//! | 503 | `weights_missing` | **decline**, quietly - installed, no model fetched yet |
//! | 507 | `out_of_memory` | **decline** - rule 9's third guard working as specified |
//!
//! A decline routes the region to rung 2 and lists it in review; a fault fails
//! the page. No reply at all within the deadline is a fault, and the child is
//! killed.
//!
//! ---
//!
//! # Rule 9, and who enforces which half
//!
//! | Guard | Rust | Python | What is actually enforced here |
//! |---|---|---|---|
//! | Bound the allocator the backend uses, in its own units | computes and checks | applies | The byte counts come from [`hardware::gate`]; the knob names come from [`backend`], per backend; the open fails on a short echo |
//! | Cap the cache, do not only purge it | computes and checks | applies | `cache_bytes` is a *number* in `limits`, and there is no purge endpoint in this protocol to reach for instead. On `sdnq` the *same* knob does both: torch's fraction bounds reserved memory, so one cap covers the live allocations and the cache ([`Cap::Fraction`]) |
//! | Fail fast, into the ladder | enforces | enforces | Python raises on the cap and answers `507`; Rust holds a per-region deadline, kills on a hang, checks the reported peak, and - [`crate::memory::Step::RefuseSidecar`] - refuses the rung outright when the kernel reports pressure |
//! | Weights are the floor, never the budget | enforces | reports | [`hardware::Demand`] has two fields and no constructor that takes one; a report with no working set is refused |
//! | One region in flight, back to the floor between | enforces / measures | enforces | `&mut self` on [`Client::render`] and `409` on the far side; [`Floor`] records the series still to be judged |

pub mod backend;
pub mod client;
pub mod hardware;
pub mod http;
pub mod models;
pub mod wire;

use std::path::{Path, PathBuf};

pub use backend::{Backend, Cap, Contract};
pub use client::{Client, Floor, Sidecar};
pub use hardware::{Budget, Demand, Verdict};

use crate::engines::model::Decline;

/// The Python module the child is started as: `python -m manga_cleaner_sidecar`.
///
/// Named here rather than at the spawn site because it is half of the contract
/// with the other repository - the interpreter path is discovered and this
/// never is.
pub const MODULE: &str = "manga_cleaner_sidecar";

/// The directory the sidecar is installed into, under every root but the
/// developer's.
pub const DIRECTORY: &str = "sidecar";

/// The virtual environment a developer's checkout keeps beside the sources.
pub const DEV_VENV: &str = ".sidecar-venv";

/// Where a found install lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// The interpreter to run. Inside the virtual environment, never the
    /// system's - a sidecar run under the wrong interpreter is a sidecar with
    /// the wrong `mlx` on its path, and the reference implementation records
    /// exactly that failure from the other direction: a partial `websockets`
    /// leaking from one environment into another's.
    pub python: PathBuf,
    /// The working directory the child is given.
    pub root: PathBuf,
}

/// Whether this machine can offer rung 3a, and if not, what to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Nothing is installed. **The ordinary state**, and the only one with
    /// nothing to report.
    Absent,
    /// Installed, and this machine will not carry it. The reason is the same
    /// vocabulary a declined region carries, so the settings panel and the
    /// review row say the same thing about the same machine.
    Declined(Decline),
    /// Installed, and admitted, with what the sidecar is allowed to hold.
    Offered { install: Install, budget: Budget },
}

impl Availability {
    /// The i18n key to show, or `None` where there is nothing to say.
    ///
    /// [`Availability::Absent`] is the `None`, and that is the whole of "a rung
    /// the user never installed is not a failure the user has to dismiss".
    pub fn reason_key(&self) -> Option<&'static str> {
        match self {
            Availability::Absent | Availability::Offered { .. } => None,
            Availability::Declined(decline) => Some(decline.reason_key()),
        }
    }

    pub fn is_offered(&self) -> bool {
        matches!(self, Availability::Offered { .. })
    }
}

/// Whether a sidecar's `GET /v1/health` report says it can run this backend.
///
/// `reported` is [`wire::Health::backends`] - the ids whose Python packages
/// import over there. The parent cannot answer this for itself: [`find`] looks
/// for a `pyvenv.cfg` and an interpreter beside it and cannot see which packages
/// are inside, and since [`Backend::Sdnq`] a venv may legitimately hold either
/// backend's dependencies or both.
///
/// **An empty list is `true`.** The field was added after the protocol shipped,
/// so an omitted one means *this sidecar does not answer the question* and not
/// *nothing is installed* - refusing on it would refuse every older sidecar,
/// including one that would have worked. The cost of the permissive reading is
/// an `ImportError` at open on a mismatched old install, which is where the
/// failure used to land anyway.
pub fn reports_backend(reported: &[String], backend: Backend) -> bool {
    reported.is_empty() || reported.iter().any(|id| id == backend.id())
}

/// Where to look for the sidecar, in order.
///
/// The same shape as [`crate::runtime::search_paths`] and
/// `run::model_search_paths`, and for the same reasons: an environment override
/// so a developer can point at a checkout, the per-user directory an install
/// writes to, then beside the
/// executable and inside the macOS bundle's `Resources`, then the working
/// directory last.
///
/// Each entry is a **root**, not an interpreter. [`interpreter_in`] is what
/// turns one into the other, because a root may be a virtual environment itself
/// or may contain one, and both layouts happen - a developer's
/// [`DEV_VENV`] is the environment, and an installed `sidecar/` directory holds
/// sources with a `.venv` inside them.
pub fn search_paths(app_data: Option<&Path>) -> Vec<PathBuf> {
    search_paths_from(std::env::var("MANGA_CLEANER_SIDECAR").ok().as_deref(), app_data)
}

/// The same, with the override passed in rather than read.
///
/// Split out for [`crate::runtime::search_paths_from`]'s reason, which was
/// learned the hard way there: `cargo test` is parallel and `set_var` is
/// global, so two tests mutating the environment to check an order is a race.
pub fn search_paths_from(explicit: Option<&str>, app_data: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(explicit) = explicit {
        if !explicit.is_empty() {
            roots.push(PathBuf::from(explicit));
        }
    }
    if let Some(dir) = app_data {
        roots.push(dir.join(DIRECTORY));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.join(DIRECTORY));
            roots.push(dir.join("../Resources").join(DIRECTORY));
        }
    }
    roots.push(PathBuf::from(DEV_VENV));
    roots.push(PathBuf::from(DIRECTORY));
    roots
}

/// The marker that says a directory is a virtual environment and not merely a
/// directory with a `bin/python3` in it.
///
/// **This check earned itself.** Without it, `interpreter_in("/usr")` answers
/// `/usr/bin/python3` - the system interpreter, with no `mlx` on its path and
/// no sidecar module - and the search order would have found it on any machine
/// whose app-data directory happened to be `/usr`. Worse, it is the *shape* of
/// the failure the reference implementation records from the other side: a
/// child started under the wrong interpreter, importing a package that is
/// partly there. A layout test is not a substitute for a marker.
pub const VENV_MARKER: &str = "pyvenv.cfg";

/// The interpreter inside a root, if the root holds a virtual environment.
///
/// Two layouts, tried in that order: the root **is** an environment, or it
/// **contains** one at `.venv`. Both are real - a developer's [`DEV_VENV`] is
/// the environment itself, and an installed `sidecar/` directory holds sources
/// with `.venv` inside them.
///
/// Nothing else is guessed at, and each candidate must carry
/// [`VENV_MARKER`] as well as an interpreter. Walking further, or trusting the
/// interpreter alone, would mean running the sidecar against whatever `python3`
/// happened to be nearby.
pub fn interpreter_in(root: &Path) -> Option<PathBuf> {
    let (dir, name) = if cfg!(windows) { ("Scripts", "python.exe") } else { ("bin", "python3") };
    for venv in [root.to_path_buf(), root.join(".venv")] {
        let python = venv.join(dir).join(name);
        if venv.join(VENV_MARKER).is_file() && python.is_file() {
            return Some(python);
        }
    }
    None
}

/// The first install on the search path, or `None`.
///
/// `None` is the ordinary answer and costs nothing: it is four `is_file` calls
/// against paths that do not exist.
pub fn find(app_data: Option<&Path>) -> Option<Install> {
    find_within(&search_paths(app_data))
}

/// [`find`], against roots the caller supplies.
pub fn find_within(roots: &[PathBuf]) -> Option<Install> {
    roots.iter().find_map(|root| {
        interpreter_in(root).map(|python| Install {
            python,
            // The root the child runs in is the one that was searched, whether
            // or not the environment turned out to be a level down. A child
            // whose working directory is its own `.venv` would resolve a
            // relative model cache inside the environment, which is a
            // directory an upgrade replaces.
            root: root.clone(),
        })
    })
}

/// Everything §4a's gate needs, asked at once: is it installed, and will this
/// machine carry it?
///
/// `demand` is what `GET /v1/health` reported, or `None` where the sidecar has
/// not been asked yet or would not say. Passing `None` gives the answer for a
/// backend that cannot be bounded, which is a refusal - see [`hardware`].
pub fn availability(app_data: Option<&Path>, demand: Option<Demand>) -> Availability {
    match find(app_data) {
        None => Availability::Absent,
        Some(install) => match hardware::gate(demand) {
            Verdict::Admitted(budget) => Availability::Offered { install, budget },
            Verdict::Declined(decline) => Availability::Declined(decline),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the order, and the developer's checkout is late in it - an
    /// installed sidecar beats one somebody happens to be standing next to.
    #[test]
    fn the_search_order_puts_the_override_first_and_the_checkout_last() {
        let app_data = PathBuf::from("/tmp/app-data");
        let paths = search_paths_from(Some("/opt/mc-sidecar"), Some(&app_data));
        assert_eq!(paths[0], PathBuf::from("/opt/mc-sidecar"));
        assert_eq!(paths[1], app_data.join(DIRECTORY));
        assert_eq!(paths[paths.len() - 2], PathBuf::from(DEV_VENV));
        assert_eq!(paths[paths.len() - 1], PathBuf::from(DIRECTORY));

        // An empty override is not a root. It is an environment variable
        // somebody exported and never set.
        assert!(!search_paths_from(Some(""), None).contains(&PathBuf::new()));
    }

    /// **Absence is quiet.** The assertion that matters is the second one:
    /// nothing to render, so nothing for a user to dismiss.
    #[test]
    fn a_machine_with_no_sidecar_is_absent_and_says_nothing() {
        let nowhere = vec![PathBuf::from("/nonexistent/manga-cleaner-sidecar")];
        assert_eq!(find_within(&nowhere), None);

        let availability = Availability::Absent;
        assert_eq!(availability.reason_key(), None);
        assert!(!availability.is_offered());
    }

    /// Every other refusal does have something to say, and each says its own
    /// thing - a machine that is too small and a machine that could not be
    /// measured have different remedies.
    #[test]
    fn every_refusal_but_absence_names_a_reason_and_no_two_share_one() {
        let refusals = [
            Decline::SidecarMachine { needed: 2, room: 1 },
            Decline::SidecarUnknownMachine,
            Decline::SidecarPlatform,
            Decline::SidecarBackendMissing,
            Decline::SidecarUnbounded,
            Decline::SidecarMemory,
            Decline::SidecarWeightsMissing,
        ];
        let mut keys: Vec<&str> = refusals
            .iter()
            .map(|d| Availability::Declined(*d).reason_key().expect("a refusal with no reason"))
            .collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count);
        for key in keys {
            // The family, and then the rung's own prefix inside it - spelled in
            // two pieces because the catalogue's completeness test scans this
            // file for literal keys, and `decline.reason.sidecar` looks exactly
            // like one.
            assert!(key.starts_with("decline.reason."), "{key}");
            assert!(key["decline.reason.".len()..].starts_with("sidecar"), "{key}");
        }
    }

    /// **An install is a venv, whichever backend's packages are in it** - and
    /// which those are is the sidecar's answer and not this side's.
    ///
    /// The permissive reading of an empty list is the assertion that matters:
    /// the field was added after the protocol shipped, and refusing on its
    /// absence would refuse every sidecar built before it.
    #[test]
    fn a_sidecar_that_names_its_importable_backends_is_believed_and_silence_is_not_a_refusal() {
        let both = vec!["mflux".to_owned(), "sdnq".to_owned()];
        for backend in [Backend::Mflux, Backend::Sdnq] {
            assert!(reports_backend(&both, backend));
        }
        // A torch-only venv: `sdnq` runs, `mflux` does not, and the refusal for
        // the second is a different sentence from "your platform has no
        // backend" - which is the whole reason the reason exists.
        let torch_only = vec!["sdnq".to_owned()];
        assert!(reports_backend(&torch_only, Backend::Sdnq));
        assert!(!reports_backend(&torch_only, Backend::Mflux));
        assert_eq!(
            Decline::SidecarBackendMissing.reason_key(),
            "decline.reason.sidecarBackend"
        );
        assert_ne!(
            Decline::SidecarBackendMissing.reason_key(),
            Decline::SidecarPlatform.reason_key()
        );

        // An older sidecar says nothing, and nothing is not "no".
        for backend in Backend::ALL {
            assert!(reports_backend(&[], backend), "{backend:?} refused on silence");
        }
    }

    /// A directory that is not a virtual environment is not an install, **and
    /// the system interpreter is not one either**.
    ///
    /// The second assertion is why [`VENV_MARKER`] exists: `/usr/bin/python3`
    /// is a real file on every machine this runs on, and an earlier revision of
    /// this function returned it.
    #[test]
    fn a_root_without_a_virtual_environment_in_it_is_not_an_install() {
        assert_eq!(interpreter_in(Path::new("/")), None);
        assert_eq!(interpreter_in(Path::new("/usr")), None, "the system interpreter is not ours");
    }

    /// Both layouts, against a directory tree built for the test: the root as
    /// the environment, and the root containing one.
    #[test]
    fn an_interpreter_is_found_whether_the_root_is_the_environment_or_holds_one() {
        let base = std::env::temp_dir().join(format!("mc-sidecar-{}", std::process::id()));
        let (dir, name) =
            if cfg!(windows) { ("Scripts", "python.exe") } else { ("bin", "python3") };

        let flat = base.join("flat");
        std::fs::create_dir_all(flat.join(dir)).unwrap();
        std::fs::write(flat.join(dir).join(name), b"").unwrap();
        // An interpreter with no marker beside it is not an environment.
        assert_eq!(interpreter_in(&flat), None);
        std::fs::write(flat.join(VENV_MARKER), b"home = /usr/bin\n").unwrap();
        assert_eq!(interpreter_in(&flat), Some(flat.join(dir).join(name)));

        let nested = base.join("nested");
        std::fs::create_dir_all(nested.join(".venv").join(dir)).unwrap();
        std::fs::write(nested.join(".venv").join(dir).join(name), b"").unwrap();
        std::fs::write(nested.join(".venv").join(VENV_MARKER), b"home = /usr/bin\n").unwrap();
        assert_eq!(interpreter_in(&nested), Some(nested.join(".venv").join(dir).join(name)));

        // The root that was searched is the working directory, not the
        // environment a level down inside it.
        let found = find_within(std::slice::from_ref(&nested)).unwrap();
        assert_eq!(found.root, nested);

        let _ = std::fs::remove_dir_all(&base);
    }
}
