//! Rung 3a's hardware gate: §4a's arithmetic,
//! against [`crate::memory`]'s answers.
//!
//! The gate for the two
//! in-process rungs was closed earlier, leaving this rung's own ceiling as a
//! residue - *"rung
//! 3a's own hardware gate is still not built … §4a's ceiling has no
//! implementation, because the rung it gates does not exist"*. The rung exists
//! now, so this is that ceiling.
//!
//! ## The arithmetic, and where each term comes from
//!
//! §4a writes it as three subtractions:
//!
//! ```text
//! budget = 0.75 × physical RAM
//!        − the resident cleaner (02's table)
//!        − headroom for the webview and the OS
//! ```
//!
//! Two of the three are already answered by [`crate::memory`], and answering
//! them a second time here is how a budget that existed twice, corrected in
//! one copy, happened. So:
//!
//! **`0.75 × physical RAM`, less the webview, is [`crate::memory::room`].** It
//! is the lesser of what the platform will grant at all
//! ([`crate::memory::Machine::process_ceiling`]) and what is actually going
//! spare right now ([`crate::memory::available`]), each already less
//! [`crate::memory::WEBVIEW_BYTES`]. Note that the fraction there is **two
//! thirds and not three quarters**, deliberately: `memory`'s own documentation
//! records that 21 GB of 32 is the low end of §4a's stated 21–24 GB range and
//! that a gate takes the low end of a range it did not measure.
//!
//! **The resident cleaner is [`RESIDENT_CLEANER`]**, and it is a citation
//! rather than a number invented here.
//!
//! **The OS headroom is already paid, twice, and this file does not invent a
//! constant for it.** Once by the fraction: two thirds of 32 GB is 21.33 GB
//! against three quarters' 24 GB, so the gate has already set aside 2.67 GB
//! that §4a's formula would have spent. And once by `available()`, which is the
//! kernel's own `memorystatus_level` and therefore *is* the OS's current
//! occupancy rather than an allowance for it. A fourth subtraction on top would
//! be a number nobody measured, sitting in front of a rung whose whole problem
//! is that its demand is unmeasured too. Recorded as a deferral rather than
//! guessed at.
//!
//! ## Weights are the floor and never the budget
//!
//! Rule 9's fourth guard is enforced by the shape of [`Demand`]: it has two
//! fields, neither is optional, and there is no constructor that takes weights
//! alone. A backend that reports `weights_bytes` and no `working_set_bytes` is
//! **refused** - [`Decline::SidecarUnbounded`] - because the alternative is to
//! gate on the floor, which §4a says in as many words will "admit machines that
//! then thrash".
//!
//! The gate also refuses a declared working set *below* the weights. The working
//! set is the process's peak with the weights inside it, so such a declaration is
//! a sidecar that has not understood the field, and the remedy is not to believe it.
//! The gate compares the peak alone with the room: it added the weights on top until
//! 2026-10-30, which counted them twice and called Klein 4B's 5.4 GiB peak 15 GiB.
//!
//! ## A backend with no render path here is declined first, and for that reason
//!
//! The gate's **first** question is not about memory at all: does the backend
//! this run is going to ask for render on this platform, in this build? It is
//! asked before the memory questions because it does not depend on their
//! answers, and the refusal is [`Decline::SidecarPlatform`] - *this build has no
//! FLUX sidecar for this kind of computer*.
//!
//! **What that question now means has changed, because the answer used to be
//! "no" for two whole platforms.** Rung 3a shipped with one rendering backend,
//! `mflux`, which is MLX, which is Apple Silicon; `sdcpp` was declared and
//! returned 501. So Windows and
//! Linux were refused outright - the platforms §4a reopened the rung *for*.
//! [`super::backend::Backend::Sdnq`] is the answer to that: torch and
//! `diffusers`, which build everywhere, so **no platform is refused for want of
//! a backend any more**. [`platform_decline`] therefore answers `None` on every
//! shipped build, and it is still computed from
//! [`super::backend::Backend::offered`] rather than replaced by a `None`,
//! because a backend removed from the table has to change the answer.
//!
//! What survives is the *per-backend* form, [`platform_decline_for`]: a user who
//! chooses `mflux` in Settings on a Windows machine has chosen something that
//! cannot run, and the honest reply names their choice rather than their memory.
//! Whether the chosen backend's Python packages are actually installed is a
//! different question with a different remedy, and it is the sidecar's to answer -
//! [`super::wire::Health::backends`], checked in
//! [`crate::engines::flux::Inpainter::open`].
//!
//! **That correction still stands underneath all of this.** Those platforms
//! used to be declined under [`Decline::SidecarUnknownMachine`], because
//! [`crate::memory::room`] answered `None` there and the unclassifiable arm
//! caught them. A probe closed that: `room` answers on both now, so what
//! reaches that arm is a probe that was made and failed.
//!
//! ## An unclassifiable machine is declined, and that is not obviously right
//!
//! [`crate::memory::room`] answers `None` where a probe was made and failed -
//! a sysctl, a `/proc` read that would not read, a `GlobalMemoryStatusEx` that
//! returned false - and on a platform with no probe at all. This gate declines
//! that machine. Four reasons, and then the argument against, which is real:
//!
//! - **The two gates' worst cases are not comparable.**
//!   [`crate::accel::choose_within`] honours a forced provider when it has no
//!   memory answer, and it is right to: underneath it sits a measured automatic
//!   choice, and the cost of being wrong is a slow run or a session that fails
//!   to build. Underneath this one sits an out-of-process child that
//!   was observed reaching 25 GB, and the
//!   cost of being wrong is the machine.
//! - **§4a asks for a positive answer, not the absence of a negative one.** The
//!   rung is "gated on detected hardware"; hardware that was not detected has
//!   not passed a gate.
//! - **Declining costs the user nothing they had.** The rung is absent by
//!   default and ships no weights; a machine that is declined is in the state
//!   every machine is in until someone installs something.
//! - **It is declined *with a reason*.** §4a's requirement is that "the gate
//!   reports what it found, so a user whose machine is declined is told why
//!   rather than shown a control that fails", and
//!   [`Decline::SidecarUnknownMachine`] is a different sentence from
//!   [`Decline::SidecarMachine`] precisely so the two remedies stay apart.
//!
//! **The objection it used to carry is now nobody's.** This paragraph once read
//! "against: this declines Windows and Linux, and those are exactly the
//! platforms §4a reopened the rung for". The probe gave both of them a `room`
//! figure, and then the backend refusal above stopped them anyway; the thing
//! owed to §4a's discrete-GPU case was a backend that runs there, and
//! [`super::backend::Backend::Sdnq`] is it. What is left in this arm is the case
//! it was always meant for: a machine whose own kernel would not say how much
//! memory it has.

use crate::engines::model::Decline;
use crate::memory::{self, MIB};

use super::backend::Backend;
use super::wire::Basis;

/// What the memory budget says this
/// application costs while it is cleaning, and therefore what rung 3a does not
/// get to spend.
///
/// **2.45 GB**, the measured peak process RSS at rung 2 -
/// the largest of its three rows, over eight copies of a twenty-region page
/// through one held session on an Apple M5. The largest row, because a
/// sidecar may be asked for a region on a page whose other regions are at rung
/// 2, and a gate that assumed the cheaper rung would be a gate for the run that
/// did not happen.
///
/// **§4a's own figure for this term is wrong,
/// and it is the smaller of the two.** It says "the resident cleaner (02's
/// table, ~1.9 GB peak with LaMa)". 1.89 GB is `spikes/strip-memory`'s peak at
/// **rungs 0 and 1** over synthetic pages with no model rung in the run at all;
/// the same document's measured-peaks table gives 2.45 GB for rung 2, and 02
/// itself says the spike "is **not** Phase 2's exit criterion" for exactly
/// this kind of reason. Using §4a's number would leave 560 MB of a Mac
/// unaccounted for in front of a multi-gigabyte child.
pub const RESIDENT_CLEANER: u64 = 2510 * MIB;

/// What share of the budget the backend's buffer cache may hold: **one eighth**.
///
/// Rule 9's second guard is "cap the cache; do not only purge it", and its
/// argument doubles as the reason the cap is low rather than generous: *"every
/// region is a different crop shape, so a cached buffer is never the size the
/// next region wants, and an uncapped cache grows with region count rather than
/// with model size."* A cache exists to serve reuse; this workload has almost
/// none, so the cap is set near the bottom of the range where an allocator
/// still has somewhere to put a scratch buffer.
///
/// **Zero was the other candidate** and it is not obviously worse - it is the
/// strictest possible reading of the same sentence, and MLX accepts it. It is
/// not taken because nobody has measured what a zero-cache diffusion loop costs
/// in time, and a rung that already spends ten to sixty seconds a region is not
/// the place to find out by accident. Recorded as a deferral.
pub const CACHE_SHARE_NUMERATOR: u64 = 1;
pub const CACHE_SHARE_DENOMINATOR: u64 = 8;

/// The room this machine has for a sidecar, and the two limits that go over the
/// wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// What [`crate::memory::room`] answered: everything a process may reach
    /// here, webview already subtracted.
    pub room: u64,
    /// `room` less [`RESIDENT_CLEANER`]. What the sidecar may hold.
    pub total: u64,
    /// Of that, the backend's buffer cache's share.
    pub cache: u64,
}

impl Budget {
    /// The budget a machine of this size implies, before anything is known
    /// about the model.
    ///
    /// Public because the *budget* and the *gate* answer different questions
    /// and only one of them needs a [`Demand`]: this is arithmetic over the
    /// room, and admission is [`gate_within`]'s. A caller that has to spawn the
    /// sidecar before it can ask what the model costs - which is every caller,
    /// because the demand comes from `GET /v1/health` - needs the limits to
    /// spawn *with*, and then gets the authoritative gate inside
    /// [`crate::engines::flux::Inpainter::open`], where the health report has
    /// arrived. Handing out a budget is not admitting anything.
    pub fn from_room(room: u64) -> Budget {
        let total = room.saturating_sub(RESIDENT_CLEANER);
        Budget {
            room,
            total,
            cache: total / CACHE_SHARE_DENOMINATOR * CACHE_SHARE_NUMERATOR,
        }
    }

    /// The limits as the wire carries them.
    pub fn limits(&self) -> super::wire::Limits {
        super::wire::Limits { total_bytes: self.total, cache_bytes: self.cache }
    }
}

/// What a model and a backend will cost, resident, if this machine runs them.
///
/// Two fields, because rule 9's fourth guard is that they are not interchangeable:
/// "what is on disk is what the model's parameters cost. What decides whether it
/// runs is that plus activations, plus the VAE's untiled peak, plus whatever the
/// runtime keeps."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Demand {
    /// The quantized weights. The **floor**.
    pub weights: u64,
    /// The process's peak: the resident weights, and everything the parameter
    /// count does not predict.
    pub working_set: u64,
    /// Whether the sidecar measured `working_set` or declared it. Carried into
    /// the provenance, never collapsed into the number.
    pub basis: Basis,
}

impl Demand {
    /// What the machine must have room for. The peak, which holds the weights.
    pub fn total(&self) -> u64 {
        self.working_set
    }

    /// A demand assembled from what `GET /v1/health` reported, or `None` when
    /// the sidecar did not report enough to be bounded.
    ///
    /// Three ways to answer `None`, and each of them is rule 9's first or
    /// fourth guard refusing to be talked out of itself: no weights figure, no
    /// working-set figure, or a working set smaller than the weights it is
    /// supposed to sit on top of.
    pub fn from_report(
        weights: Option<u64>,
        working_set: Option<u64>,
        basis: Option<Basis>,
    ) -> Option<Demand> {
        let (weights, working_set) = (weights?, working_set?);
        if working_set < weights {
            return None;
        }
        Some(Demand { weights, working_set, basis: basis.unwrap_or(Basis::Declared) })
    }
}

/// What the gate found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The machine carries it, and here is what the sidecar is allowed.
    Admitted(Budget),
    /// It does not, and here is why - in the same vocabulary a declined region
    /// is reported in, so the settings panel and the review row say the same
    /// thing about the same machine.
    Declined(Decline),
}

impl Verdict {
    pub fn budget(self) -> Option<Budget> {
        match self {
            Verdict::Admitted(budget) => Some(budget),
            Verdict::Declined(_) => None,
        }
    }

    pub fn reason_key(self) -> Option<&'static str> {
        match self {
            Verdict::Admitted(_) => None,
            Verdict::Declined(decline) => Some(decline.reason_key()),
        }
    }
}

/// Whether **any** backend in [`super::backend`]'s table renders on the platform
/// this was compiled for.
///
/// `true` on every shipped build, because [`Backend::Sdnq`] is torch and torch
/// builds everywhere - and computed from the table rather than written as `true`,
/// so removing a backend changes the answer instead of leaving a stale constant
/// behind. It was a `const HAS_BACKEND` reading "macOS on Apple Silicon, and
/// nothing else" for as long as `mflux` was the only arm.
pub fn has_any_backend() -> bool {
    !Backend::offered().is_empty()
}

/// Whether one named backend renders here. [`Backend::has_render_path`], which
/// carries the per-backend reasoning.
pub fn has_backend(backend: Backend) -> bool {
    backend.has_render_path()
}

/// The reason rung 3a is not on offer here **at all**, or `None` where it is.
///
/// For callers that gate *before* they have a demand to gate with - a settings
/// panel being drawn, a picker deciding whether to show the rung - and which
/// would otherwise have to say [`Decline::SidecarUnknownMachine`] or nothing.
///
/// Answers `None` on every shipped build now. Kept, rather than deleted at its
/// call sites, because "does this platform have a backend at all" is still the
/// question those call sites are asking and the answer is still derived rather
/// than assumed.
pub fn platform_decline() -> Option<Decline> {
    (!has_any_backend()).then_some(Decline::SidecarPlatform)
}

/// The reason **this backend** is not on offer here, or `None`.
///
/// The form that still fires: a user who picked `mflux` in Settings on a machine
/// with no MLX, or a build asked for the `sdcpp` stub, is told that their choice
/// has no render path - not that their memory could not be measured.
pub fn platform_decline_for(backend: Backend) -> Option<Decline> {
    (!has_backend(backend)).then_some(Decline::SidecarPlatform)
}

/// The gate, against this machine.
pub fn gate(demand: Option<Demand>) -> Verdict {
    gate_within(demand, memory::room())
}

/// The gate, against a room figure the caller supplies.
///
/// Split out for [`crate::accel::choose_within`]'s reason: the arithmetic is
/// the whole of the decision, and a machine of a given size is not a state a
/// test can arrange.
pub fn gate_within(demand: Option<Demand>, room: Option<u64>) -> Verdict {
    gate_for(has_any_backend(), demand, room)
}

/// [`gate_within`], for the backend the caller is actually going to open.
///
/// The authoritative form for [`crate::engines::flux::Inpainter::open`]: a run
/// asks for one backend, and the platform question that matters is that one's.
pub fn gate_backend_within(
    backend: Backend,
    demand: Option<Demand>,
    room: Option<u64>,
) -> Verdict {
    gate_for(has_backend(backend), demand, room)
}

/// [`gate_within`], with the platform's own answer supplied too.
///
/// The whole gate as a **pure function of its three signals**, which is what
/// lets the tests below assert what a Windows machine is told from a host that
/// is not one.
pub fn gate_for(has_backend: bool, demand: Option<Demand>, room: Option<u64>) -> Verdict {
    // First, and before the demand: a platform with no backend is refused
    // whatever the model costs and whatever the machine has, and telling the
    // user their sidecar "cannot say how much memory it needs" when the truth
    // is that this build cannot render on their machine at all is a refusal
    // that names the wrong remedy.
    if !has_backend {
        return Verdict::Declined(Decline::SidecarPlatform);
    }
    let Some(demand) = demand else {
        return Verdict::Declined(Decline::SidecarUnbounded);
    };
    // Then the two memory answers, and this way round: a machine nobody could
    // measure is a different sentence from a machine that was measured and is
    // too small, and the remedies differ - one is "your kernel would not say"
    // and the other is "this Mac is not large enough".
    let Some(room) = room else {
        return Verdict::Declined(Decline::SidecarUnknownMachine);
    };
    let budget = Budget::from_room(room);
    let needed = demand.total();
    if needed > budget.total {
        return Verdict::Declined(Decline::SidecarMachine { needed, room: budget.total });
    }
    Verdict::Admitted(budget)
}

/// What this gate would say about a machine of a given physical size, with
/// nothing else running.
///
/// For the settings panel's "why was I declined" and for the tests. It uses the
/// **ceiling** rather than [`crate::memory::room`]'s live term, so it answers a
/// question about the machine rather than about the moment.
pub fn ceiling_of(physical: u64) -> u64 {
    let working_set =
        physical / memory::WORKING_SET_DENOMINATOR * memory::WORKING_SET_NUMERATOR;
    working_set.saturating_sub(memory::WEBVIEW_BYTES).saturating_sub(RESIDENT_CLEANER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::GIB;

    fn demand(weights: u64, working_set: u64) -> Option<Demand> {
        Demand::from_report(Some(weights), Some(working_set), Some(Basis::Declared))
    }

    /// **Weights are the floor and never the budget**, as arithmetic. Measured
    /// figures: 4.6 GB of quantized weights on a 32 GB machine, against the
    /// ~25 GB the reference actually reached. A gate written against the floor
    /// admits it; this one does not.
    #[test]
    fn a_gate_written_against_the_weights_would_admit_what_this_one_refuses() {
        let room = ceiling_of(32 * GIB) + RESIDENT_CLEANER;
        let weights = 4600 * MIB;

        // The floor alone fits with room to spare, which is the trap.
        assert!(matches!(
            gate_for(true, demand(weights, weights), Some(room)),
            Verdict::Admitted(_)
        ));

        // The figure observed does not, and the refusal carries both
        // numbers so the user is told what was found rather than that
        // something was.
        match gate_for(true, demand(weights, 25 * GIB), Some(room)) {
            Verdict::Declined(Decline::SidecarMachine { needed, room }) => {
                assert_eq!(needed, 25 * GIB);
                assert!(room < needed);
            }
            other => panic!("{other:?}"),
        }
    }

    /// The weights are inside the peak, so a model is not charged for them twice.
    /// Klein 4B on SDNQ: 5.47 GB on disk, a peak of 5.43 GiB on an L4.
    #[test]
    fn the_peak_alone_is_what_a_machine_needs_room_for() {
        let want = demand(5476 * MIB, 8 * GIB).unwrap();
        assert_eq!(want.total(), 8 * GIB);
        let room = 8 * GIB + RESIDENT_CLEANER;
        assert!(matches!(gate_for(true, Some(want), Some(room)), Verdict::Admitted(_)));
    }

    /// A backend that will not say what its working set is cannot be bounded,
    /// and rule 9's answer to that is to refuse it - not to fall back to the
    /// weights.
    #[test]
    fn a_demand_that_names_no_working_set_is_refused_rather_than_defaulted() {
        let room = Some(64 * GIB);
        assert_eq!(
            gate_for(true, Demand::from_report(Some(4 * GIB), None, None), room),
            Verdict::Declined(Decline::SidecarUnbounded)
        );
        assert_eq!(
            gate_for(true, Demand::from_report(None, Some(8 * GIB), None), room),
            Verdict::Declined(Decline::SidecarUnbounded)
        );
        // A working set under the weights is a sidecar that has misunderstood
        // the field. Believing it would be worse than refusing it.
        assert_eq!(
            gate_for(true, Demand::from_report(Some(8 * GIB), Some(GIB), None), room),
            Verdict::Declined(Decline::SidecarUnbounded)
        );
    }

    /// The unclassifiable machine, and it is told apart from the small one.
    /// See the module docs for why this is the answer and what it costs.
    ///
    /// **What reaches this arm has changed.** It used to be every Windows and
    /// Linux machine, because [`crate::memory::room`] answered `None` there;
    /// a probe means it is now a probe that was made and failed, which is
    /// what the reason string always claimed it was.
    #[test]
    fn a_machine_that_cannot_be_measured_is_declined_under_its_own_reason() {
        assert_eq!(
            gate_for(true, demand(GIB, GIB), None),
            Verdict::Declined(Decline::SidecarUnknownMachine)
        );
        assert_ne!(
            Decline::SidecarUnknownMachine.reason_key(),
            Decline::SidecarMachine { needed: 0, room: 0 }.reason_key(),
            "the two refusals have different remedies and must read differently"
        );
    }

    /// **A Windows or Linux machine is told the truth**, which is that this
    /// build has no backend for it - not that its memory could not be measured,
    /// which was the answer it used to get and has not been true for a while.
    ///
    /// Asserted from a host that is neither, which is the whole reason
    /// [`gate_for`] takes the platform as an argument: a machine with a
    /// backend and a machine without one are two values of one parameter, and
    /// both are reachable from any host.
    #[test]
    fn a_platform_with_no_backend_is_refused_for_that_and_not_for_its_memory() {
        // Plenty of room, a perfectly bounded demand - and still refused, on
        // the one ground that no amount of memory answers.
        assert_eq!(
            gate_for(false, demand(GIB, GIB), Some(64 * GIB)),
            Verdict::Declined(Decline::SidecarPlatform)
        );
        // And it beats every other refusal to the answer, because it is the one
        // whose remedy is not the user's to apply.
        assert_eq!(
            gate_for(false, None, None),
            Verdict::Declined(Decline::SidecarPlatform),
            "an unbounded demand on an unmeasured machine is still, first, a platform with no backend"
        );
        // Three distinct sentences for three distinct remedies.
        for other in [
            Decline::SidecarUnknownMachine.reason_key(),
            Decline::SidecarUnbounded.reason_key(),
            Decline::SidecarMachine { needed: 0, room: 0 }.reason_key(),
        ] {
            assert_ne!(Decline::SidecarPlatform.reason_key(), other);
        }
    }

    /// The compile-time half, on this host. [`has_any_backend`] is what
    /// `gate_within` fills in and [`platform_decline`] is the same fact for
    /// callers that gate before they have a demand - the two must never
    /// disagree - and [`platform_decline_for`] is the per-backend form that
    /// still fires.
    #[test]
    fn the_platform_answer_is_one_fact_and_not_two() {
        assert_eq!(platform_decline().is_none(), has_any_backend());
        assert_eq!(
            gate_within(demand(GIB, GIB), Some(64 * GIB)) == Verdict::Declined(Decline::SidecarPlatform),
            !has_any_backend()
        );
        // **And it is `true` here and on every other platform**, because the
        // portable arm builds everywhere. This assertion is the one that would
        // have failed before `sdnq` existed on a Windows host.
        assert!(has_any_backend(), "no backend renders in this build at all");
        assert_eq!(platform_decline(), None);

        // The per-backend question is the one with two answers. `mflux` is MLX
        // and MLX is Apple Silicon; `sdcpp` renders nowhere; `sdnq` renders
        // here, whatever "here" is.
        assert_eq!(
            platform_decline_for(Backend::Mflux).is_none(),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
        assert_eq!(platform_decline_for(Backend::Sdcpp), Some(Decline::SidecarPlatform));
        assert_eq!(platform_decline_for(Backend::Sdnq), None);
        assert_eq!(
            gate_backend_within(Backend::Sdcpp, demand(GIB, GIB), Some(64 * GIB)),
            Verdict::Declined(Decline::SidecarPlatform),
            "a backend with no render path is refused before the arithmetic"
        );
        assert!(matches!(
            gate_backend_within(Backend::Sdnq, demand(GIB, GIB), Some(64 * GIB)),
            Verdict::Admitted(_)
        ));
    }

    /// §4a's own worked example: "an 8 GB unified-memory Mac does not [run it
    /// comfortably], and the rung must not be offered there".
    ///
    /// It is confirmed by the arithmetic rather than asserted. Two thirds of
    /// 8 GB is 5.33 GB; less the webview and less the resident cleaner, **2.77
    /// GB** is left - which is below FLUX.2 Klein 4B's 4.6 GB of quantized
    /// weights before a single activation, so the smallest honest demand this
    /// rung has is already more than the machine.
    #[test]
    fn an_eight_gigabyte_machine_is_never_offered_the_rung() {
        let budget = ceiling_of(8 * GIB);
        assert!(budget > 0, "there is room, and it is not enough room");
        assert!(budget < 4600 * MIB, "{} MB is over the weights alone", budget / MIB);
        assert!(matches!(
            gate_for(true, demand(4600 * MIB, 4600 * MIB), Some(budget + RESIDENT_CLEANER)),
            Verdict::Declined(Decline::SidecarMachine { .. })
        ));
    }

    /// And the machine measured: 32 GB, where the same model has room and the
    /// ~25 GB the reference
    /// reached does not (which is
    /// `a_gate_written_against_the_weights_would_admit_what_this_one_refuses`).
    #[test]
    fn a_thirty_two_gigabyte_machine_has_room_for_the_model_a_forty_six_gigabyte_one_does_not() {
        let budget = ceiling_of(32 * GIB);
        assert!(budget > 9200 * MIB, "{} MB", budget / MIB);
        assert!(matches!(
            gate_for(true, demand(4600 * MIB, 4600 * MIB), Some(budget + RESIDENT_CLEANER)),
            Verdict::Admitted(_)
        ));
    }

    /// The cache cap is a fraction of the budget and it is never the budget,
    /// which is the difference between capping a cache and having one.
    #[test]
    fn the_cache_is_capped_well_below_the_total_it_sits_inside() {
        let budget = Budget::from_room(24 * GIB);
        assert_eq!(budget.total, 24 * GIB - RESIDENT_CLEANER);
        assert_eq!(budget.cache, budget.total / 8);
        assert!(budget.cache < budget.total / 4);
        let limits = budget.limits();
        assert_eq!(limits.total_bytes, budget.total);
        assert_eq!(limits.cache_bytes, budget.cache);
    }

    /// Not an assertion about how much memory this machine has - the shape
    /// [`crate::memory`]'s own runtime test takes. Only that the gate answers,
    /// that the answer is consistent, and that the figures are printed so a
    /// reader of the test log can see what this machine was told.
    ///
    /// This is [`gate_within`] and therefore the *real* gate, platform included:
    /// on a build with no backend the whole match falls to the first arm, and
    /// the memory arms below are the macOS-on-Apple-Silicon ones.
    #[test]
    fn the_gate_answers_on_this_machine_and_says_what_it_found() {
        let machine = memory::machine();
        let room = memory::room();
        // FLUX.2 Klein 4B at 4 bits, from the disk table, with a working set
        // the same size again - a placeholder for a figure still owed, and
        // marked `Declared` so it can never be read as measured.
        let want = demand(4600 * MIB, 4600 * MIB);
        let verdict = gate_within(want, room);
        eprintln!(
            "backends {:?}, physical {:?} MB, room {:?} MB, resident cleaner {} MB, verdict {:?}",
            Backend::offered().iter().map(|b| b.id()).collect::<Vec<_>>(),
            machine.physical.map(|b| b / MIB),
            room.map(|b| b / MIB),
            RESIDENT_CLEANER / MIB,
            verdict,
        );
        if !has_any_backend() {
            assert_eq!(verdict, Verdict::Declined(Decline::SidecarPlatform));
            return;
        }
        match (room, verdict) {
            (None, verdict) => {
                assert_eq!(verdict, Verdict::Declined(Decline::SidecarUnknownMachine))
            }
            (Some(room), Verdict::Admitted(budget)) => {
                assert_eq!(budget.room, room);
                assert!(budget.total <= room);
                assert!(budget.total >= want.unwrap().total());
            }
            (Some(_), Verdict::Declined(decline)) => {
                assert!(matches!(decline, Decline::SidecarMachine { .. }), "{decline:?}");
            }
        }
    }
}
