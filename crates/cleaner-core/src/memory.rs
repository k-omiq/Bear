//! How much memory this machine will actually give us, asked rather than
//! assumed.
//!
//! This module fills a hole: *"Nothing queries either number:
//! `cleaner_core::accel` probes execution providers, not memory."* The two
//! numbers are physical memory, and
//! the working set the platform will actually grant, which on macOS is
//! `recommendedMaxWorkingSetSize` and is emphatically **not** total RAM.
//!
//! This module is written to the same standard [`crate::accel`] is: it **asks
//! the system**, the way [`crate::accel::available`] asks the loaded ONNX
//! Runtime which providers it was built with rather than inferring them from
//! the platform. Where it cannot ask, it says so - every answer here is an
//! `Option`, and [`Machine::basis`] records how the answer was arrived at, so a
//! caller can tell a number the kernel reported from a number this file
//! computed from a fraction written down in a design document.
//!
//! ## What can be asked, on macOS
//!
//! Through `sysctlbyname`, which is in libSystem and therefore already linked -
//! this workspace carries no `libc` dependency and does not gain one for four
//! integers:
//!
//! | key | what it answers |
//! |---|---|
//! | `hw.memsize` | physical memory, exactly |
//! | `kern.memorystatus_vm_pressure_level` | the kernel's own pressure state: normal, warn, critical |
//! | `kern.memorystatus_level` | the percentage of memory the kernel counts as available |
//! | `vm.page_free_count` × `hw.pagesize` | free pages, which is a floor on the above and not the same number |
//!
//! ## What cannot be asked, and is therefore stated as a fraction
//!
//! **`recommendedMaxWorkingSetSize` is a Metal property, not a sysctl.**
//! Reading it means a `MTLDevice`, which means an Objective-C runtime binding
//! this crate does not have and would not otherwise want. So the working set is
//! [`WORKING_SET_NUMERATOR`]/[`WORKING_SET_DENOMINATOR`] of physical memory and
//! is reported under [`Basis::Fraction`] rather than [`Basis::Reported`], which
//! is the difference between a measurement and an arithmetic claim and is
//! exactly that distinction.
//!
//! The fraction is **two thirds and not three quarters**, which is the low end
//! of the platform's own range: *"roughly 75% of
//! physical memory - about 21–24 GB of a 32 GB machine"*. 21 of 32 is 65.6%,
//! 24 of 32 is 75%, and a gate takes the low end of a range it did not measure.
//! Being generous here admits a machine that then thrashes, and
//! the rule is explicit about
//! which of the two failures is the one that matters: *"a runaway must fail
//! fast, into the ladder … rather than being absorbed into the memory
//! compressor, which loses the machine."* Declining is cheap.
//!
//! ## `os_proc_available_memory` returns zero here, and that is not a bug
//!
//! `os_proc_available_memory`
//! is "the obvious one on macOS" for a pressure signal. It was tried first and
//! it does not work for this application: the function reports the room left
//! under a **per-process** memory limit, and a plain macOS process has no such
//! limit, so it answers `0` - measured on the reference M5, not inferred. Zero
//! is indistinguishable from "no room at all", which is the worst possible
//! reading for a signal whose job is to trigger a ladder, so nothing here calls
//! it. The signal used instead is the kernel's own system-wide pressure level,
//! which is what `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` delivers and what
//! the pressure ladder is actually about.
//!
//! ## Windows and Linux
//!
//! Both are asked now. What the silence cost was recorded -
//! *"both platforms run the ladder with no memory
//! policy at all, which is a different thing from running it with a
//! conservative one"* - and the answer is the same shape as the macOS one: ask
//! the system, widen to bytes, record the basis.
//!
//! | | physical | available | free | pressure |
//! |---|---|---|---|---|
//! | **macOS** | `hw.memsize` | `kern.memorystatus_level` × physical | `vm.page_free_count` × `hw.pagesize` | `kern.memorystatus_vm_pressure_level`, the kernel's own three states |
//! | **Linux** | `/proc/meminfo` `MemTotal` | `/proc/meminfo` `MemAvailable` | `/proc/meminfo` `MemFree` | `/proc/pressure/memory` (PSI) `some`/`full` `avg10`, else derived from the available fraction |
//! | **Windows** | `GlobalMemoryStatusEx` `ullTotalPhys` | `ullAvailPhys` | none - see below | derived from `dwMemoryLoad` |
//!
//! Units and semantics are the same everywhere: bytes, and *available* always
//! means memory the kernel counts as obtainable without paging anything out -
//! `MemAvailable` and `ullAvailPhys` are both that number, not free pages.
//!
//! **Three fidelity differences, and they are differences in the pressure
//! signal only.**
//!
//! 1. **macOS is the only platform that reports pressure rather than having it
//!    derived.** `kern.memorystatus_vm_pressure_level` is the kernel's own
//!    verdict - the same one `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` delivers.
//!    Nothing on the other two answers that question directly.
//! 2. **Linux answers a near-equivalent where PSI is present.** Pressure stall
//!    information is a measurement of *time lost to memory*, which is closer to
//!    what the ladder wants than any occupancy figure: [`PSI_WARN_SOME_AVG10`]
//!    and its two companions are thresholds on it, and they are this file's
//!    numbers rather than the kernel's. PSI needs `CONFIG_PSI` and a 4.20
//!    kernel; where the file is absent - old kernels, and containers that do
//!    not mount it - the fraction below is used instead.
//! 3. **Windows, and Linux without PSI, are a fraction of occupancy and
//!    nothing more.** [`AVAILABLE_WARN_PERCENT`] and
//!    [`AVAILABLE_CRITICAL_PERCENT`] over available-against-physical. That is
//!    weaker than either of the above: it says the machine is *full*, not that
//!    anything is *stalling*, and a machine can be full and perfectly happy.
//!    It is taken because the alternative is [`Pressure::Unknown`] forever,
//!    which is a ladder that never steps, and that is judged the worse of the
//!    two.
//!
//! **Windows has no `free()` and answers `None` there.** `MEMORYSTATUSEX` has
//! no free-pages field distinct from `ullAvailPhys`, and reporting the same
//! number under two names would make a diagnostics line say something it does
//! not know. `free` is a diagnostic and never a gate ([`free`]), so nothing
//! downstream changes shape.
//!
//! **Where a platform still cannot answer, `None` still means *do not act*.**
//! A `/proc` that will not read and a `GlobalMemoryStatusEx` that fails both
//! land back on [`Basis::Unmeasured`] and [`Pressure::Unknown`], and the
//! callers are written so that is *no signal* rather than *no room* -
//! [`crate::accel::choose_within`] honours a forced provider when it has no
//! memory answer, and [`Ladder`] takes no step.
//!
//! **None of this has been run on either platform.** The parsing and the
//! derivation are unit-tested here against fixture strings, which is a test of
//! the arithmetic and not of the kernel; neither machine has ever run this,
//! and two of the three pressure signals are derived rather than reported.

#[cfg(target_os = "macos")]
use std::ffi::c_void;
use std::sync::OnceLock;

/// One mebibyte, and one gibibyte, because every number below is written in
/// them and `1024 * 1024 * 1024` five times is a typo waiting to happen.
pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;

/// The working set, as a fraction of physical memory: **two thirds**. See the
/// module docs for why it is the low end of the platform's range and not the
/// middle of it.
pub const WORKING_SET_NUMERATOR: u64 = 2;
pub const WORKING_SET_DENOMINATOR: u64 = 3;

/// The budget table's top of the
/// webview's range. It is not in this process and it is on this machine, which
/// is the whole argument about a sidecar applied to the thing that
/// hosts us.
pub const WEBVIEW_BYTES: u64 = 180 * MIB;

/// Available memory, as a percentage of physical, at or below which a machine
/// that has no better signal is called [`Pressure::Warn`] - and then
/// [`Pressure::Critical`].
///
/// Used on Windows (over `dwMemoryLoad`, which is occupancy, so the available
/// share is `100 − load`) and on a Linux without PSI. **These are this file's
/// numbers, not any kernel's**, and they are set where they are because the
/// thing on the other side of the ladder is a 510 MB session and a
/// multi-gigabyte child: a fifth of a 16 GB machine is 3.2 GB, which is about
/// one rung-2 session plus the room to build the next one, and a tenth is under
/// it. Below that the next allocation this application makes is the one that
/// starts paging.
///
/// They are deliberately a *pair* rather than one threshold with a hysteresis:
/// [`Ladder`] latches, so a reading only ever has to be right once.
pub const AVAILABLE_WARN_PERCENT: u64 = 20;
pub const AVAILABLE_CRITICAL_PERCENT: u64 = 10;

/// Linux PSI thresholds, on the ten-second average.
///
/// `some avg10` is the share of the last ten seconds in which **at least one**
/// task was stalled waiting on memory; `full avg10` is the share in which
/// **every** runnable task was, which is a machine that made no progress at
/// all. The three numbers:
///
/// - [`PSI_WARN_SOME_AVG10`] - a tenth of the window with something stalled.
///   Ten per cent is the figure the `oomd` family treats as the bottom of
///   "meaningfully short", and it is a warning here rather than a kill.
/// - [`PSI_CRITICAL_SOME_AVG10`] - `systemd-oomd`'s own default
///   `MemoryPressureLimit` is 60%, and that is the number it kills on. This
///   file only unloads a session at it.
/// - [`PSI_CRITICAL_FULL_AVG10`] - a tenth of the window with *nothing*
///   progressing is already a machine in trouble, so `full` reaches critical an
///   order of magnitude sooner than `some` does.
pub const PSI_WARN_SOME_AVG10: f64 = 10.0;
pub const PSI_CRITICAL_SOME_AVG10: f64 = 60.0;
pub const PSI_CRITICAL_FULL_AVG10: f64 = 10.0;

/// How an answer was arrived at. A number the kernel gave us and a number this
/// file derived from a design document are not the same kind of thing, and a
/// gate that cannot tell them apart cannot report honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// The platform was asked for the working set itself and answered.
    ///
    /// **Nothing produces this today**, and the variant exists because the
    /// difference it names is the one that matters: reading Metal's
    /// `recommendedMaxWorkingSetSize` needs an Objective-C binding this crate
    /// does not have, so macOS answers [`Basis::Fraction`] instead. A caller
    /// that treats the two the same is a caller that will report an arithmetic
    /// claim as a measurement.
    Reported,
    /// The platform answered for physical memory, and the working set is
    /// [`WORKING_SET_NUMERATOR`]/[`WORKING_SET_DENOMINATOR`] of it - this
    /// file's arithmetic over the platform's number.
    Fraction,
    /// Nothing was asked, because nothing here knows how to ask on this
    /// platform - or it was asked and the platform would not answer.
    Unmeasured,
}

/// What the machine is, as far as this build can establish it.
///
/// Physical memory does not change while the process runs and neither does the
/// fraction, so this is computed once - [`machine`] caches it. Anything that
/// *does* change is a separate call ([`pressure`], [`available`]) and is
/// deliberately not a field here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine {
    /// Physical memory, in bytes.
    pub physical: Option<u64>,
    /// What the platform will let one process hold resident before it starts
    /// paying for it somewhere else.
    pub working_set: Option<u64>,
    /// How `working_set` was arrived at.
    pub basis: Basis,
}

impl Machine {
    /// The room a *whole process* has, which is the working set less the
    /// webview that is not in this process and is on this machine.
    ///
    /// Every figure this is compared against is a **peak process RSS**, so the
    /// core's own sessions and the runtime's allocator arena are already inside
    /// the demand and must not be subtracted here as well.
    pub fn process_ceiling(&self) -> Option<u64> {
        self.working_set.map(|set| set.saturating_sub(WEBVIEW_BYTES))
    }
}

/// The kernel's own memory pressure state.
///
/// Three values and an unknown, matching `kern.memorystatus_vm_pressure_level`,
/// which carries the same values `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`
/// delivers: 1 normal, 2 warn, 4 critical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    Normal,
    Warn,
    Critical,
    /// Not asked, or asked and not answered. **Not** a synonym for `Normal`:
    /// callers treat it as "no signal", which means they take no step rather
    /// than assuming the machine is fine.
    Unknown,
}

impl Pressure {
    /// Whether the kernel is telling us to give memory back.
    pub fn is_elevated(self) -> bool {
        matches!(self, Pressure::Warn | Pressure::Critical)
    }
}

/// The machine, asked once.
pub fn machine() -> Machine {
    static MACHINE: OnceLock<Machine> = OnceLock::new();
    *MACHINE.get_or_init(probe_machine)
}

/// Memory the kernel currently counts as available, in bytes.
///
/// This is the kernel's own accounting and not free pages: `memorystatus_level`
/// is a percentage that includes what the system can reclaim without paging
/// anything out, which is the number that decides whether an allocation is
/// cheap. Free pages alone would decline a machine whose memory is doing its
/// job - a large file cache is not a machine under pressure.
///
/// `None` where it cannot be asked.
pub fn available() -> Option<u64> {
    probe_available()
}

/// Free pages, in bytes. A floor on [`available`] and a different number: it
/// excludes everything the kernel could reclaim.
///
/// Reported in diagnostics rather than used as a gate, because a gate on this
/// declines every machine that has ever opened a large file.
///
/// `None` on Windows, which has no such counter - see the module docs.
pub fn free() -> Option<u64> {
    probe_free()
}

/// The kernel's pressure level, now - or, where the kernel has no such verdict
/// to give, this file's derivation of one. The module docs' table says which is
/// which per platform, and the three fidelity notes beneath it say what the
/// difference costs.
pub fn pressure() -> Pressure {
    probe_pressure()
}

/// How much a process may reach for **right now**, in bytes.
///
/// The lesser of two ceilings, because they answer different questions and a
/// demand has to clear both:
///
/// - [`Machine::process_ceiling`] is what the platform will grant at all. It
///   does not move while the process runs.
/// - [`available`] less the webview is what is actually going spare. It moves
///   constantly, and it is the difference between "this machine could carry
///   this" and "this machine could carry this *while the user has a browser
///   open*".
///
/// `None` when neither can be established - a sysctl that failed, a `/proc`
/// that would not read, a `GlobalMemoryStatusEx` that returned false, or a
/// platform that is none of the three. It is no longer a synonym for "not
/// macOS", and it still means *no answer*, never *no room*.
pub fn room() -> Option<u64> {
    let ceiling = machine().process_ceiling();
    let now = available().map(|bytes| bytes.saturating_sub(WEBVIEW_BYTES));
    match (ceiling, now) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (answer, None) | (None, answer) => answer,
    }
}

/* ------------------------------------------------------------------ */
/* The pressure ladder                                                 */
/* ------------------------------------------------------------------ */

/// The pressure ladder, as a decision rather than as prose.
///
/// > **Memory pressure has a defined order**, and the order is the budget's:
/// > unload rung 3a, unload LaMa, drop to one decode window in flight. Each
/// > step is recoverable - the region routes down the ladder and the run
/// > continues.
///
/// The steps are ordered largest-and-most-optional first and each one is
/// *recoverable*: dropping rung 2's session costs the run the rung - regions
/// route down to rung 0 and are listed in review - and it does not cost
/// the run.
///
/// **The first step is rung 3a's, and it is only a step where there is a
/// sidecar to refuse.** An earlier revision of this file started one step down
/// with the note "rung 3a does not exist in this build"; it does now
/// ([`crate::sidecar`]), and the order above is restored. But a step that gives
/// nothing back is not a step: an automatic run never holds a sidecar
/// (rung 3a is reachable per region and
/// never from `runClean`), and announcing `RefuseSidecar` there would spend the
/// first elevated reading on a process that was never spawned and leave rung 2's
/// 510 MB resident until the second. So [`Ladder::holding_sidecar`] is what puts
/// the step in the ladder, and a caller that never calls it gets exactly the
/// two-step ladder that was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Refuse and unload rung 3a - the sidecar. It is spawned on first use
    /// and unloaded under pressure, ahead of LaMa, because it is both the
    /// largest resident thing and the only optional one.
    ///
    /// It is *refuse* as well as *unload*, and both halves are latched by
    /// [`Ladder::may_open_sidecar`]: a machine that has just asked for memory
    /// back is not a machine to spawn a multi-gigabyte child on, and the step
    /// undoing itself one region later is the failure this whole ladder is
    /// shaped to avoid.
    RefuseSidecar,
    /// Give back rung 2's session. The largest thing this process holds by
    /// choice, and the only one a run can continue without.
    UnloadInpainter,
    /// The remedy: one window in flight rather than two.
    OneWindowInFlight,
}

/// What the pressure ladder is standing on, and which step it is on.
///
/// Held by the caller across a run - the step is a **latch**, not a sample.
/// Pressure that came and went still means the machine was short, and a run
/// that climbed back up the ladder the moment the reading improved would spend
/// its time rebuilding a 2.1 s session. Steps are given back when the run ends,
/// which is when the sessions go anyway.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ladder {
    taken: Option<Step>,
    sidecar: bool,
}

impl Ladder {
    pub fn new() -> Ladder {
        Ladder { taken: None, sidecar: false }
    }

    /// Tell the ladder that rung 3a's sidecar is running, so
    /// [`Step::RefuseSidecar`] becomes the first step. See [`Step`] for why this
    /// is declared rather than assumed.
    pub fn holding_sidecar(&mut self) {
        self.sidecar = true;
    }

    /// Whether rung 3a may be spawned at all.
    ///
    /// **Any** step down this ladder closes it, not only [`Step::RefuseSidecar`]:
    /// a machine that made us give rung 2's 510 MB back is not a machine to
    /// start a multi-gigabyte child on, and the ordering says as much by
    /// putting the sidecar above LaMa.
    pub fn may_open_sidecar(&self) -> bool {
        self.taken.is_none()
    }

    /// The step this ladder is on, if any.
    pub fn step(&self) -> Option<Step> {
        self.taken
    }

    /// Ask the machine, and answer with the step to take **now** - `None` when
    /// there is nothing to do, either because the machine is fine or because
    /// this platform cannot say.
    ///
    /// Called between regions, which is the only place a step is safe to take:
    /// rule 6's discipline means the previous region's buffers are already gone
    /// and the next one's do not exist yet, so what is resident at this moment
    /// is the sessions, and the sessions are what the ladder gives back.
    ///
    /// **An unknown reading takes no step.** `Pressure::Unknown` is a platform
    /// with no probe at all, and it is also a probe that failed - a sysctl, a
    /// `/proc` read, a `GlobalMemoryStatusEx` - and in every case acting would
    /// be acting on nothing.
    pub fn poll(&mut self) -> Option<Step> {
        self.take(pressure())
    }

    /// [`Ladder::poll`] against a reading the caller supplies, so the ladder's
    /// order is testable without a machine under pressure - which is not a
    /// state a test can arrange, and is the reason this is a separate function
    /// rather than a mock.
    pub fn take(&mut self, pressure: Pressure) -> Option<Step> {
        if !pressure.is_elevated() {
            return None;
        }
        let next = match self.taken {
            None if self.sidecar => Step::RefuseSidecar,
            None => Step::UnloadInpainter,
            Some(Step::RefuseSidecar) => Step::UnloadInpainter,
            Some(Step::UnloadInpainter) => Step::OneWindowInFlight,
            // The bottom. There is no third step in this build, and inventing
            // one - refusing the page, failing the run - would break the
            // property that makes this ladder safe: every step is recoverable.
            Some(Step::OneWindowInFlight) => return None,
        };
        self.taken = Some(next);
        Some(next)
    }

    /// Whether the run should be holding rung 2's session at all.
    ///
    /// [`Step::RefuseSidecar`] does **not** close this: it is the step that
    /// exists so that rung 2 survives one elevated reading longer than the
    /// optional rung above it.
    pub fn may_hold_inpainter(&self) -> bool {
        !matches!(self.taken, Some(Step::UnloadInpainter | Step::OneWindowInFlight))
    }

    /// Rule 6's concurrency, after whatever steps have been taken.
    pub fn windows_in_flight(&self, from_ram: usize) -> usize {
        match self.taken {
            Some(Step::OneWindowInFlight) => 1,
            _ => from_ram,
        }
    }
}

/// The i18n key a step is reported under. No English crosses the seam, and a
/// step the user is not told about is a run that silently got slower.
pub fn step_key(step: Step) -> &'static str {
    match step {
        Step::RefuseSidecar => "notice.memory.refusedSidecar",
        Step::UnloadInpainter => "notice.memory.unloadedInpainter",
        Step::OneWindowInFlight => "notice.memory.oneWindow",
    }
}

/* ------------------------------------------------------------------ */
/* Deriving, which is the same everywhere                              */
/* ------------------------------------------------------------------ */

/// The [`Machine`] a physical-memory answer implies.
///
/// **One copy, for all three platforms.** The fraction and the basis are this
/// file's arithmetic over the platform's number, and the platform has no say in
/// either; three copies of `physical / 3 * 2` would be three places for the
/// fraction to drift.
fn machine_from(physical: Option<u64>) -> Machine {
    Machine {
        physical,
        working_set: physical
            .map(|bytes| bytes / WORKING_SET_DENOMINATOR * WORKING_SET_NUMERATOR),
        basis: match physical {
            Some(_) => Basis::Fraction,
            None => Basis::Unmeasured,
        },
    }
}

/// Pressure from occupancy: what is left, against what there is.
///
/// The weakest of the three signals and the one two platforms fall back to -
/// see the module docs. A physical figure of zero is not a full machine, it is
/// a machine nobody measured, so it answers [`Pressure::Unknown`] rather than
/// dividing by it.
// Not called on macOS, and called on every platform by the tests below -
// which is why it is an `allow` and not a `cfg`: a function that only
// compiled where it is called could only be tested where it is called, and
// this host is not that platform.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
fn pressure_from_available(available: u64, physical: u64) -> Pressure {
    if physical == 0 {
        return Pressure::Unknown;
    }
    // Integer arithmetic over bytes, so a 64 GB machine does not overflow the
    // multiply: `available` is at most `physical`, and `physical / 100` cannot
    // be scaled past it.
    let percent = available / (physical / 100).max(1);
    if percent <= AVAILABLE_CRITICAL_PERCENT {
        Pressure::Critical
    } else if percent <= AVAILABLE_WARN_PERCENT {
        Pressure::Warn
    } else {
        Pressure::Normal
    }
}

/// One `/proc/meminfo` field, in bytes.
///
/// The file's own unit is `kB`, meaning kibibytes, and it is written on every
/// memory line; a line without it is a count rather than a size and is returned
/// as it stands. Parsing rather than an index because the set of lines differs
/// between kernels and `MemAvailable` in particular is 3.14 and later.
///
/// Pure, and tested against fixture strings on a machine that has no `/proc` -
/// which is the whole reason it is a function of a `&str`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn meminfo_bytes(text: &str, field: &str) -> Option<u64> {
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else { continue };
        if name.trim() != field {
            continue;
        }
        let mut parts = rest.split_whitespace();
        let value: u64 = parts.next()?.parse().ok()?;
        return match parts.next() {
            Some("kB") | Some("KB") | Some("kb") => Some(value.saturating_mul(1024)),
            None => Some(value),
            // A unit this function does not know is not a unit to guess at.
            Some(_) => None,
        };
    }
    None
}

/// One `avg10` out of `/proc/pressure/memory`.
///
/// The file is two lines, `some` and `full`, each `avg10= avg60= avg300=
/// total=`. A kernel without `CONFIG_PSI` has no file at all, and a cgroup v1
/// container has the host's rather than its own - neither is this function's
/// problem, because both arrive here as "no text" or "no line".
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn psi_avg10(text: &str, kind: &str) -> Option<f64> {
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some(kind) {
            continue;
        }
        for field in fields {
            if let Some(value) = field.strip_prefix("avg10=") {
                return value.parse().ok();
            }
        }
    }
    None
}

/// PSI's two lines, as a [`Pressure`].
///
/// `None` where the text carries no `some` line at all, which is the caller's
/// signal to fall back to occupancy rather than to report a calm machine it did
/// not measure. A `full` line that is missing is not that: `some` alone is a
/// complete reading, and `full` only ever raises the answer.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn pressure_from_psi(text: &str) -> Option<Pressure> {
    let some = psi_avg10(text, "some")?;
    let full = psi_avg10(text, "full").unwrap_or(0.0);
    if some >= PSI_CRITICAL_SOME_AVG10 || full >= PSI_CRITICAL_FULL_AVG10 {
        Some(Pressure::Critical)
    } else if some >= PSI_WARN_SOME_AVG10 {
        Some(Pressure::Warn)
    } else {
        Some(Pressure::Normal)
    }
}

/* ------------------------------------------------------------------ */
/* Asking the platform: macOS                                          */
/* ------------------------------------------------------------------ */

#[cfg(target_os = "macos")]
fn probe_machine() -> Machine {
    machine_from(sysctl_u64("hw.memsize"))
}

#[cfg(target_os = "macos")]
fn probe_available() -> Option<u64> {
    let physical = machine().physical?;
    let percent = sysctl_u64("kern.memorystatus_level")?;
    if percent > 100 {
        return None;
    }
    Some(physical / 100 * percent)
}

#[cfg(target_os = "macos")]
fn probe_free() -> Option<u64> {
    let pages = sysctl_u64("vm.page_free_count")?;
    let page = sysctl_u64("hw.pagesize")?;
    Some(pages.saturating_mul(page))
}

#[cfg(target_os = "macos")]
fn probe_pressure() -> Pressure {
    match sysctl_u64("kern.memorystatus_vm_pressure_level") {
        Some(1) => Pressure::Normal,
        Some(2) => Pressure::Warn,
        Some(4) => Pressure::Critical,
        _ => Pressure::Unknown,
    }
}

/* ------------------------------------------------------------------ */
/* Asking the platform: Linux                                          */
/* ------------------------------------------------------------------ */

/// The kernel's memory summary. Read afresh every time rather than cached:
/// `MemAvailable` is the moving half of [`room`] and a cached one would be the
/// bug [`machine`]'s cache exists to avoid the other way round.
#[cfg(target_os = "linux")]
const MEMINFO: &str = "/proc/meminfo";

/// Pressure stall information, 4.20 and later with `CONFIG_PSI=y`. Absent is an
/// ordinary answer, not an error.
#[cfg(target_os = "linux")]
const PSI_MEMORY: &str = "/proc/pressure/memory";

#[cfg(target_os = "linux")]
fn meminfo(field: &str) -> Option<u64> {
    meminfo_bytes(&std::fs::read_to_string(MEMINFO).ok()?, field)
}

#[cfg(target_os = "linux")]
fn probe_machine() -> Machine {
    machine_from(meminfo("MemTotal"))
}

/// `MemAvailable`, which is the kernel's own estimate of what can be had
/// without swapping - reclaimable page cache included. It is `kern.memorystatus_level`'s
/// counterpart and emphatically not `MemFree`.
#[cfg(target_os = "linux")]
fn probe_available() -> Option<u64> {
    meminfo("MemAvailable")
}

#[cfg(target_os = "linux")]
fn probe_free() -> Option<u64> {
    meminfo("MemFree")
}

#[cfg(target_os = "linux")]
fn probe_pressure() -> Pressure {
    if let Ok(text) = std::fs::read_to_string(PSI_MEMORY) {
        if let Some(pressure) = pressure_from_psi(&text) {
            return pressure;
        }
    }
    match (probe_available(), machine().physical) {
        (Some(available), Some(physical)) => pressure_from_available(available, physical),
        _ => Pressure::Unknown,
    }
}

/* ------------------------------------------------------------------ */
/* Asking the platform: Windows                                        */
/* ------------------------------------------------------------------ */

/// `GlobalMemoryStatusEx`, as the three fields this file uses: total physical,
/// available physical, and the load percentage.
///
/// One call rather than three, because the three are one snapshot and a caller
/// that mixed two snapshots could report more available than physical.
///
/// The struct is zeroed and then told its own length, which is the documented
/// calling convention - `GlobalMemoryStatusEx` fails rather than answering if
/// `dwLength` is not `sizeof(MEMORYSTATUSEX)`, and that failure is the `None`
/// here.
#[cfg(windows)]
fn memory_status() -> Option<(u64, u64, u32)> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // Returns a `BOOL`: zero is failure, and `GetLastError` would say why. It
    // is not asked, for the reason `sysctl_u64` does not ask `errno` - the
    // caller's vocabulary is `Option`, and a machine that will not say how much
    // memory it has is one case however it got there.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return None;
    }
    Some((status.ullTotalPhys, status.ullAvailPhys, status.dwMemoryLoad))
}

#[cfg(windows)]
fn probe_machine() -> Machine {
    machine_from(memory_status().map(|(total, _, _)| total))
}

#[cfg(windows)]
fn probe_available() -> Option<u64> {
    memory_status().map(|(_, available, _)| available)
}

/// Windows has no free-pages counter distinct from `ullAvailPhys`, so this
/// says so rather than reporting the same number twice. See the module docs.
#[cfg(windows)]
fn probe_free() -> Option<u64> {
    None
}

/// `dwMemoryLoad` is *occupancy* - "the approximate percentage of physical
/// memory that is in use" - so the available share is its complement, and the
/// thresholds are the same ones a Linux without PSI uses.
#[cfg(windows)]
fn probe_pressure() -> Pressure {
    match memory_status() {
        // Guard the complement rather than trusting it: a load above 100 is not
        // a number this function knows how to read.
        Some((_, _, load)) if load <= 100 => {
            pressure_from_available(100 - u64::from(load), 100)
        }
        _ => Pressure::Unknown,
    }
}

/* ------------------------------------------------------------------ */
/* Asking the platform: everywhere else                                */
/* ------------------------------------------------------------------ */

/// The BSDs, and anything else. Nothing here claims to know how to ask, and
/// `None` means *no answer* rather than *no room* - which is the same contract
/// every platform above answers under when its own probe fails.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod elsewhere {
    use super::{machine_from, Machine, Pressure};

    pub(super) fn probe_machine() -> Machine {
        machine_from(None)
    }
    pub(super) fn probe_available() -> Option<u64> {
        None
    }
    pub(super) fn probe_free() -> Option<u64> {
        None
    }
    pub(super) fn probe_pressure() -> Pressure {
        Pressure::Unknown
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use elsewhere::{probe_available, probe_free, probe_machine, probe_pressure};

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sysctlbyname(
        name: *const u8,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *const c_void,
        newlen: usize,
    ) -> i32;
}

/// One integer sysctl, widened to `u64`.
///
/// The width is the kernel's business and it differs between keys -
/// `hw.memsize` is 64-bit and `kern.memorystatus_level` is 32 - so the size is
/// read back rather than assumed, and anything that is not four or eight bytes
/// is not an integer and answers `None`.
#[cfg(target_os = "macos")]
fn sysctl_u64(name: &str) -> Option<u64> {
    let name = format!("{name}\0");
    let mut buffer = [0u8; 8];
    let mut length = buffer.len();
    let code = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr() as *mut c_void,
            &mut length,
            std::ptr::null(),
            0,
        )
    };
    if code != 0 {
        return None;
    }
    match length {
        4 => Some(u32::from_ne_bytes(buffer[..4].try_into().ok()?) as u64),
        8 => Some(u64::from_ne_bytes(buffer)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not an assertion about how much memory this machine has - that is the
    /// machine's business, and it is exactly the shape
    /// [`crate::accel`]'s own runtime test takes. Only that asking does not
    /// panic, that the answers are consistent with each other, and that the
    /// basis is recorded.
    #[test]
    fn the_machine_answers_or_says_it_cannot() {
        let machine = machine();
        match machine.physical {
            Some(physical) => {
                assert!(physical >= GIB, "a machine with under a gigabyte is not one this runs on");
                let set = machine.working_set.expect("physical without a working set");
                assert!(set < physical, "the working set is a fraction of physical, not all of it");
                assert_eq!(machine.basis, Basis::Fraction);
                assert!(machine.process_ceiling().unwrap() < set);
                eprintln!(
                    "physical {} MB, working set {} MB, ceiling {} MB, available {:?} MB, free {:?} MB, {:?}",
                    physical / MIB,
                    set / MIB,
                    machine.process_ceiling().unwrap() / MIB,
                    available().map(|b| b / MIB),
                    free().map(|b| b / MIB),
                    pressure(),
                );
            }
            None => {
                assert_eq!(machine.basis, Basis::Unmeasured);
                assert_eq!(machine.working_set, None);
                assert_eq!(room(), None, "a machine we cannot ask about has no room, not zero room");
            }
        }
    }

    /// The fraction, as arithmetic rather than as a sentence. A 32 GB machine
    /// lands inside the documented range of 21-24 GB, and at the bottom of
    /// it.
    #[test]
    fn the_working_set_is_the_low_end_of_the_documented_range() {
        let physical = 32 * GIB;
        let set = physical / WORKING_SET_DENOMINATOR * WORKING_SET_NUMERATOR;
        assert!((21 * GIB..=24 * GIB).contains(&set), "{} GB", set / GIB);
        assert!(set < physical * 3 / 4, "the low end of the range, not the high one");
    }

    /// **The ladder's order is the budget's**, and each step is taken once.
    ///
    /// This is the ladder an automatic run climbs, and it has two steps because
    /// an automatic run never holds a sidecar to refuse.
    #[test]
    fn the_ladder_unloads_the_inpainter_first_and_then_drops_to_one_window() {
        let mut ladder = Ladder::new();
        assert!(ladder.may_hold_inpainter());
        assert_eq!(ladder.windows_in_flight(2), 2);

        assert_eq!(ladder.take(Pressure::Warn), Some(Step::UnloadInpainter));
        assert!(!ladder.may_hold_inpainter());
        // Rule 6's remedy is the *second* step, so the first one does not take
        // it: unloading 510 MB and halving the prefetch at the same moment
        // would make it impossible to say which of them helped.
        assert_eq!(ladder.windows_in_flight(2), 2);

        assert_eq!(ladder.take(Pressure::Critical), Some(Step::OneWindowInFlight));
        assert_eq!(ladder.windows_in_flight(2), 1);

        // The bottom, and it stays there rather than inventing a third step.
        assert_eq!(ladder.take(Pressure::Critical), None);
        assert_eq!(ladder.step(), Some(Step::OneWindowInFlight));
    }

    /// A machine that is not under pressure is left alone, and so is a machine
    /// that cannot be asked.
    #[test]
    fn a_normal_or_unknown_reading_takes_no_step() {
        for reading in [Pressure::Normal, Pressure::Unknown] {
            let mut ladder = Ladder::new();
            assert_eq!(ladder.take(reading), None, "{reading:?}");
            assert!(ladder.may_hold_inpainter());
            assert_eq!(ladder.windows_in_flight(2), 2);
        }
    }

    /// The latch. Pressure that came and went still means the machine was
    /// short, and a run that climbed back would rebuild a 2.1 s session for
    /// every dip.
    #[test]
    fn a_step_is_not_given_back_when_the_reading_improves() {
        let mut ladder = Ladder::new();
        assert_eq!(ladder.take(Pressure::Warn), Some(Step::UnloadInpainter));
        assert_eq!(ladder.take(Pressure::Normal), None);
        assert!(!ladder.may_hold_inpainter(), "the step was given back on one good reading");
    }

    /// The order in full, on a run that *is* holding rung 3a: "refuse and
    /// unload rung 3a, then unload LaMa, then drop to one window in
    /// flight".
    #[test]
    fn a_run_holding_the_sidecar_refuses_it_before_it_touches_the_inpainter() {
        let mut ladder = Ladder::new();
        ladder.holding_sidecar();
        assert!(ladder.may_open_sidecar());

        assert_eq!(ladder.take(Pressure::Warn), Some(Step::RefuseSidecar));
        assert!(!ladder.may_open_sidecar());
        // The point of the step: rung 2 is still there after it. Giving back the
        // optional rung and the default inpainter on one reading would make it
        // impossible to say which of them the machine actually needed.
        assert!(ladder.may_hold_inpainter());
        assert_eq!(ladder.windows_in_flight(2), 2);

        assert_eq!(ladder.take(Pressure::Critical), Some(Step::UnloadInpainter));
        assert!(!ladder.may_hold_inpainter());
        assert_eq!(ladder.take(Pressure::Critical), Some(Step::OneWindowInFlight));
        assert_eq!(ladder.take(Pressure::Critical), None);
    }

    /// **Any** step closes the sidecar, not only its own. A machine that has
    /// just made us give rung 2 back is not a machine to spawn a
    /// multi-gigabyte child on.
    #[test]
    fn a_ladder_that_has_moved_at_all_will_not_open_the_sidecar() {
        let mut ladder = Ladder::new();
        assert!(ladder.may_open_sidecar());
        assert_eq!(ladder.take(Pressure::Warn), Some(Step::UnloadInpainter));
        assert!(!ladder.may_open_sidecar());
    }

    /* -- the platform-independent half, against fixture strings -------- */
    //
    // Every test below is of arithmetic over text, and none of it is of a
    // kernel: this host is macOS and has no `/proc` and no `MEMORYSTATUSEX`.
    // That is the split the module is written to - the platform code is four
    // small `cfg` functions that fetch, and everything that *decides* is here
    // and runs on any machine.

    /// A real `/proc/meminfo` head, from a 32 GB machine. `kB` is kibibytes,
    /// which is the file's own unit and not this file's assumption.
    const MEMINFO: &str = "\
MemTotal:       32784872 kB
MemFree:         1234560 kB
MemAvailable:   20971520 kB
Buffers:          123456 kB
Cached:         10000000 kB
SwapTotal:       2097152 kB
HugePages_Total:       0
";

    #[test]
    fn meminfo_is_read_by_name_and_widened_to_bytes() {
        assert_eq!(meminfo_bytes(MEMINFO, "MemTotal"), Some(32_784_872 * 1024));
        assert_eq!(meminfo_bytes(MEMINFO, "MemAvailable"), Some(20_971_520 * 1024));
        assert_eq!(meminfo_bytes(MEMINFO, "MemFree"), Some(1_234_560 * 1024));
        // A count is not a size, and it is not multiplied as though it were.
        assert_eq!(meminfo_bytes(MEMINFO, "HugePages_Total"), Some(0));
        // The prefix of a field name is not the field.
        assert_eq!(meminfo_bytes(MEMINFO, "Mem"), None);
        assert_eq!(meminfo_bytes(MEMINFO, "Shmem"), None);
        // `MemAvailable` is 3.14 and later. An older kernel's file has no such
        // line, and the answer is "not there" rather than a number.
        assert_eq!(meminfo_bytes("MemTotal:  4096 kB\n", "MemAvailable"), None);
        // Neither a unit this function does not know nor a value that is not a
        // number is guessed at.
        assert_eq!(meminfo_bytes("MemTotal:  4096 MB\n", "MemTotal"), None);
        assert_eq!(meminfo_bytes("MemTotal:  lots kB\n", "MemTotal"), None);
        assert_eq!(meminfo_bytes("", "MemTotal"), None);
    }

    /// The working set and the basis are the same arithmetic whatever platform
    /// answered - which is the point of there being one copy of it.
    #[test]
    fn a_physical_answer_becomes_the_same_machine_whoever_gave_it() {
        let machine = machine_from(Some(32 * GIB));
        assert_eq!(machine.working_set, Some(32 * GIB / 3 * 2));
        assert_eq!(machine.basis, Basis::Fraction);

        let unmeasured = machine_from(None);
        assert_eq!(unmeasured.working_set, None);
        assert_eq!(unmeasured.basis, Basis::Unmeasured);
        assert_eq!(unmeasured.process_ceiling(), None);
    }

    /// PSI, which is Linux's near-equivalent of the macOS kernel's own verdict:
    /// time lost to memory rather than memory occupied.
    #[test]
    fn psi_reads_the_ten_second_average_off_both_lines() {
        let calm = "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n\
                    full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(psi_avg10(calm, "some"), Some(0.0));
        assert_eq!(pressure_from_psi(calm), Some(Pressure::Normal));

        // A tenth of the window with something stalled is a warning.
        let stalling = "some avg10=12.34 avg60=3.00 avg300=1.00 total=99\n\
                        full avg10=0.10 avg60=0.00 avg300=0.00 total=4\n";
        assert_eq!(psi_avg10(stalling, "some"), Some(12.34));
        assert_eq!(psi_avg10(stalling, "full"), Some(0.10));
        assert_eq!(pressure_from_psi(stalling), Some(Pressure::Warn));

        // `full` reaches critical an order of magnitude before `some` does: a
        // tenth of the window with *nothing* progressing is already trouble,
        // and `some` is still only at the warning threshold here.
        let thrashing = "some avg10=15.00 avg60=9.00 avg300=4.00 total=900\n\
                         full avg10=11.00 avg60=6.00 avg300=2.00 total=400\n";
        assert_eq!(pressure_from_psi(thrashing), Some(Pressure::Critical));

        // And `some` alone gets there at systemd-oomd's own kill threshold.
        let dying = "some avg10=61.00 avg60=40.00 avg300=20.00 total=9000\n";
        assert_eq!(pressure_from_psi(dying), Some(Pressure::Critical));

        // No `some` line is *no reading*, not a calm machine - the caller falls
        // back to occupancy rather than reporting a verdict nobody gave.
        assert_eq!(pressure_from_psi(""), None);
        assert_eq!(pressure_from_psi("full avg10=1.00 total=4\n"), None);
        assert_eq!(pressure_from_psi("some total=4\n"), None);
    }

    /// The weakest of the three signals, and the one Windows and a PSI-less
    /// Linux stand on. The thresholds are this file's, so they are asserted as
    /// this file's rather than as a kernel's.
    #[test]
    fn occupancy_crosses_at_a_fifth_and_then_at_a_tenth() {
        let physical = 32 * GIB;
        let at = |percent: u64| pressure_from_available(physical / 100 * percent, physical);
        assert_eq!(at(100), Pressure::Normal);
        assert_eq!(at(AVAILABLE_WARN_PERCENT + 1), Pressure::Normal);
        assert_eq!(at(AVAILABLE_WARN_PERCENT), Pressure::Warn);
        assert_eq!(at(AVAILABLE_CRITICAL_PERCENT + 1), Pressure::Warn);
        assert_eq!(at(AVAILABLE_CRITICAL_PERCENT), Pressure::Critical);
        assert_eq!(at(0), Pressure::Critical);

        // Windows arrives here as `100 − dwMemoryLoad` over 100, and the two
        // routes must agree or the same machine would read differently on two
        // platforms.
        assert_eq!(pressure_from_available(100 - 95, 100), Pressure::Critical);
        assert_eq!(pressure_from_available(100 - 85, 100), Pressure::Warn);
        assert_eq!(pressure_from_available(100 - 40, 100), Pressure::Normal);

        // A machine with no physical figure is not a full machine.
        assert_eq!(pressure_from_available(0, 0), Pressure::Unknown);
        // The largest machine this could see does not overflow the arithmetic.
        assert_eq!(pressure_from_available(u64::MAX / 2, u64::MAX), Pressure::Normal);
    }

    /// A Linux fixture end to end, as far as the parsing goes: `/proc/meminfo`
    /// in, a ladder-ready reading out, on a kernel with no PSI.
    #[test]
    fn a_meminfo_with_no_psi_beside_it_still_produces_a_reading() {
        let total = meminfo_bytes(MEMINFO, "MemTotal").unwrap();
        let available = meminfo_bytes(MEMINFO, "MemAvailable").unwrap();
        // 20 GB available of 32 is a machine going about its business.
        assert_eq!(pressure_from_available(available, total), Pressure::Normal);

        // The same machine with the cache gone and nothing reclaimable.
        let squeezed = "MemTotal: 32784872 kB\nMemAvailable: 1000000 kB\n";
        let total = meminfo_bytes(squeezed, "MemTotal").unwrap();
        let available = meminfo_bytes(squeezed, "MemAvailable").unwrap();
        assert_eq!(pressure_from_available(available, total), Pressure::Critical);

        // And the ladder acts on it, which is the whole point of the exercise:
        // these platforms used to run the ladder with no policy.
        let mut ladder = Ladder::new();
        assert_eq!(
            ladder.take(pressure_from_available(available, total)),
            Some(Step::UnloadInpainter)
        );
    }

    #[test]
    fn every_step_names_a_key() {
        for step in [Step::RefuseSidecar, Step::UnloadInpainter, Step::OneWindowInFlight] {
            assert!(step_key(step).starts_with("notice.memory."), "{step:?}");
        }
    }
}
