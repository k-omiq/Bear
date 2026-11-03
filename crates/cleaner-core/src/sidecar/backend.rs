//! Which allocator each backend actually uses, and what bounding it means
//! there.
//!
//! Rule 9's first guard is the
//! one the reference implementation broke three times over, and its conclusion
//! is a sentence about *naming*: **"a guard that does not name its backend is a
//! guard for one backend. Adding a backend means adding its bound, or refusing
//! to offer it."** This module is that sentence as a table. **Three** backends
//! are in scope, no two of them share an allocator, and nothing here is written
//! in a way that could quietly apply to more than one.
//!
//! ## What went wrong in the reference, restated as requirements
//!
//! The reference implementation had four guards that were each
//! correct for the backend they were written against and silently absent
//! everywhere else. Read from this side they are four *names*, and every one of
//! them is in [`Contract::required`] for the backend it belongs to:
//!
//! | What was missing | The name here | The backend it is real on |
//! |---|---|---|
//! | `mx.set_memory_limit` had no caller | `mx.set_memory_limit` | `mflux` only - MLX |
//! | `mx.set_cache_limit` had no caller, so the buffer cache was unbounded | `mx.set_cache_limit` | `mflux` only |
//! | VAE tiling applied through an attribute the MLX inpainter does not expose | `vae_tiling` | `sdcpp` only - see below |
//! | the Qwen3-4B text encoder was never evicted after encoding | `text_encoder_eviction` | `sdcpp` only - see below |
//!
//! The two that are `mflux` only are the two whose *units* are MLX's. The two
//! below them are behaviours rather than byte counts, and each backend reaches
//! them by its own route - `stable-diffusion.cpp` through `--vae-tiling` and
//! `--clip-on-cpu`. The wire names them backend-neutrally for exactly that
//! reason: what this process requires is the *effect*, and which call produces
//! it is the sidecar's business.
//!
//! ## Why `mflux` no longer requires either behaviour
//!
//! Both were required of it and both have been withdrawn, on separate
//! measurements and for the same rule - **a guard a backend cannot promise for
//! every render is a guard it must not echo**, which is the earlier finding
//! read forwards instead of backwards.
//!
//! - `text_encoder_eviction` was applied by `mflux`'s `MemorySaver`, which nulls
//!   the text encoder before the denoising loop and cannot rebuild it. FLUX.2
//!   re-encodes the prompt on every call, so the *second* render through one
//!   open threw. The rung worked
//!   around that by reopening the model per region; the encoder is now kept
//!   instead, and what pays for it is an honest declaration - the sidecar
//!   declares 10 GiB of working set against 9.77 GB, which is what the largest
//!   shape its untiled arm can reach actually measured.
//! - `vae_tiling` helps a large crop's decode spike and **hurts a small one**:
//!   `mflux` tiles the encode above 512 px and the decode above a 512 px output,
//!   blending tiles with a cosine ramp, which is a seam across exactly the
//!   working resolution the sidecar now upscales small crops to. It is therefore
//!   applied per render, on the crop, and a per-render decision cannot be echoed
//!   at open as though it held for all of them.
//!
//! What bounds `mflux` is what is left: the two MLX setters - one of which
//! is recorded as a guideline rather than a wall - and the parent's per-region
//! peak check, which is [`Cap::Parent`]'s bound doing double duty on a backend
//! that also has setters.
//!
//! ## The three shapes a bound comes in
//!
//! MLX has a pair of setters, torch has a fraction, and ggml has nothing, and
//! pretending otherwise is how the reference ended up capping torch on a
//! torch-free path. So [`Cap`] has three variants and the differences are
//! stated rather than smoothed over. What makes the last one a bound and not a
//! hope is that **the parent enforces it in every case**: the sidecar reports
//! its peak on every reply, and [`super::Client::render`] refuses and shuts the
//! child down when that peak passes the budget. For `mflux` and `sdnq` that is a
//! second line of defence behind a setter; for `stable-diffusion.cpp` it is the
//! only one, and saying so is the point.
//!
//! ## `sdnq` is the portable arm, and the earlier guard finally lands on torch
//!
//! §4a reopened this rung for the machine with a discrete GPU, and that machine
//! cannot run a line of MLX. [`Backend::Sdnq`] is `torch` + `diffusers` with
//! `sdnq`'s 4-bit weights: CUDA, Intel XPU or Metal, which is every platform
//! this application builds for. Two guards, and each is real on every device it
//! will accept:
//!
//! - **[`MEMORY_FRACTION`]** - `torch.cuda.set_per_process_memory_fraction` and
//!   `torch.mps.set_per_process_memory_fraction`. It bounds *reserved* memory,
//!   which is the live allocations and the caching allocator's cache **together** -
//!   so rule 9's second guard, "cap the cache, do not only purge it", is met
//!   by this one knob rather than by a second one torch does not have. That is
//!   why [`Cap::Fraction`] names one setter and not two, and why requiring a
//!   cache name here would be the reference's error in a new place.
//!
//!   `PYTORCH_MPS_HIGH_WATERMARK_RATIO` is this call's ancestor. The
//!   complaint there was never that the guard was wrong; it was that it was
//!   aimed at an allocator that was not in the inference path. On this backend
//!   torch *is* the inference path.
//! - **[`DEVICE_PLACEMENT`]** - the pipeline placed for what the accelerator
//!   has free. Whole, by `pipeline.to`, on a discrete GPU with room for every
//!   component and a margin; else `diffusers`' `enable_model_cpu_offload`, one
//!   component on the device at a time; else `enable_sequential_cpu_offload`
//!   where the card cannot hold one component. Unified memory always offloads,
//!   since its card memory is RAM. Named `cpu_offload` until 2026-11-03, when a
//!   card that held the whole model still had every weight moved across the bus
//!   for every render.
//!
//! **And the two behaviours `sdcpp` requires are deliberately not required of
//! it**, on the same rule that withdrew them from `mflux`. `vae_tiling` seams a
//! crop at the working resolution a small region is upscaled into, because
//! `diffusers` tiles above its VAE's own sample threshold and that threshold is
//! below 768. `text_encoder_eviction` is not a *second* promise: the placement
//! decides it - offloaded, `diffusers` moves the encoder back to the host after
//! encoding and moves it out again next call, which is the behaviour `mflux`
//! was found unable to repeat, and held whole, the card had room for it - so it
//! is [`DEVICE_PLACEMENT`] under another name, and naming one behaviour twice
//! makes an echo look richer than it is.
//!
//! **A machine with no accelerator is refused rather than served.** With no
//! CUDA, XPU or Metal device there is nothing to offload to and no fraction to
//! set, so the sidecar answers `501 unbounded_backend` at open instead of
//! echoing guards it did not apply. A 4B transformer at four bits on a CPU is
//! minutes per step; the refusal costs nothing anybody wanted, and it is a
//! *decline* into rung 2 rather than a fault.
//!
//! ## What is deliberately not here
//!
//! **An address-space rlimit on the child.** It is the obvious way to bound a
//! host-heap allocator from the outside without the child's cooperation, and it
//! is wrong: `RLIMIT_AS` counts reserved virtual address space, which a modern
//! allocator takes in tens of gigabytes it never faults in, so a limit sized to
//! this rung's budget would kill a process that is nowhere near it. A guard
//! that fires on a number unrelated to residency is the earlier mistake in a
//! new place, and the remedy is the peak check above,
//! which is measured in the same units the budget is.

/// The backends rung 3a knows how to ask.
///
/// Three, on an explicit decision, and they are not variations of one thing:
/// MLX on Apple Silicon, torch everywhere, and GGUF through ggml. Everything
/// below differs between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `mflux` - MLX, Apple Silicon, weights in unified memory. The backend
    /// measured, and the one whose
    /// 25 GB on 4.6 GB of weights is the reason rule 9 exists.
    Mflux,
    /// `sdnq` - `torch` + `diffusers`, 4-bit SDNQ weights, on CUDA, Intel XPU or
    /// Metal. **The portable arm**, and the one that answers
    /// §4a's discrete-GPU case: there the
    /// weights occupy VRAM and never enter this process's resident set at all.
    /// It runs on Apple Silicon too, which is why it is a *choice* and not a
    /// fallback - see `src-tauri`'s `flux_backend`.
    Sdnq,
    /// `stable-diffusion.cpp` - GGUF through ggml. Portable in principle and
    /// **declared only**: the Python side declines every open with
    /// `501 unbounded_backend`, because ggml exposes no allocator setter there.
    /// Kept in the table because
    /// the contract is the honest place to record that, and because
    /// [`Backend::has_render_path`] is what stops it being offered.
    Sdcpp,
}

/// How a backend's allocator is bounded, in that allocator's own units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    /// The allocator has setters, and they are named. The sidecar calls them
    /// with the byte counts this process computed and echoes both names back.
    Allocator {
        /// The knob that bounds total allocation.
        limit: &'static str,
        /// The knob that bounds the buffer cache. Rule 9's second guard is that
        /// this is a *cap* and not a purge, and a cap is a knob with a number
        /// in it - which is why this is a separate name from `limit` rather
        /// than a call to empty something.
        cache: &'static str,
    },
    /// The allocator takes a **fraction of the device's own capacity** and
    /// nothing else. One name, on purpose: the fraction bounds *reserved*
    /// memory, so the allocator's cache is inside the same cap and there is no
    /// second knob to require. Requiring one anyway would be a name nothing can
    /// answer to, which is the failure [`Cap::Parent`]'s comment describes.
    ///
    /// The byte count the gate computed is converted to a fraction by the
    /// sidecar, because only the sidecar knows what the device has.
    Fraction {
        /// The knob, spelled backend-neutrally. Which of
        /// `torch.cuda.set_per_process_memory_fraction` /
        /// `torch.mps.set_per_process_memory_fraction` actually ran is echoed
        /// alongside it, so the log names the call and the contract names the
        /// effect.
        limit: &'static str,
    },
    /// The allocator has no setter. The bound is the parent's: the peak the
    /// sidecar reports is checked against the budget after every region and the
    /// child is shut down when it passes.
    ///
    /// This is a real bound and a coarser one. It cannot stop an allocation, it
    /// can only refuse the next region and give the memory back - which is
    /// still rule 9's third guard, *"a runaway must fail fast, into the
    /// ladder"*, one region later than a setter would manage it.
    Parent,
}

/// One backend's memory contract.
pub struct Contract {
    /// The wire name. What `POST /v1/open` sends and what `GET /v1/health`
    /// answers.
    pub id: &'static str,
    /// The allocator inference actually runs through. Written down because the
    /// reference's whole failure was a guard aimed at an allocator that was not
    /// in the inference path.
    pub allocator: &'static str,
    pub cap: Cap,
    /// The guards this process will not run this backend without. The sidecar
    /// echoes what it applied and an open with anything missing is refused.
    pub required: &'static [&'static str],
}

/// The two behaviours `stable-diffusion.cpp` must have, whatever it calls them.
///
/// They were required of both backends and are now required of one; the module
/// docs carry why. Named as constants because a typo in a contract would be a
/// guard silently not required - the failure mode this whole module is about.
pub const VAE_TILING: &str = "vae_tiling";
pub const TEXT_ENCODER_EVICTION: &str = "text_encoder_eviction";

/// MLX's own two setters, spelled as MLX spells them.
pub const MX_MEMORY_LIMIT: &str = "mx.set_memory_limit";
pub const MX_CACHE_LIMIT: &str = "mx.set_cache_limit";

/// `torch`'s two, spelled backend-neutrally because the call's namespace is the
/// *device's* and a contract is compiled before the device is known.
///
/// [`MEMORY_FRACTION`] is `torch.{cuda,mps,xpu}.set_per_process_memory_fraction`
/// and [`DEVICE_PLACEMENT`] is `pipeline.to`, `diffusers`' `enable_model_cpu_offload`
/// or its sequential sibling. The sidecar echoes both these names *and* the concrete
/// call it made, so nothing is hidden by the neutrality.
pub const MEMORY_FRACTION: &str = "memory_fraction";
pub const DEVICE_PLACEMENT: &str = "device_placement";

pub const MFLUX: Contract = Contract {
    id: "mflux",
    allocator: "mlx",
    cap: Cap::Allocator { limit: MX_MEMORY_LIMIT, cache: MX_CACHE_LIMIT },
    // Two of the four earlier guards, in the order that entry lists them. The other two are
    // withdrawn - the module docs give the measurement behind each - and what
    // stands in for them is the parent's per-region peak check plus a working
    // set declared for a model that keeps its encoder.
    required: &[MX_MEMORY_LIMIT, MX_CACHE_LIMIT],
};

pub const SDNQ: Contract = Contract {
    id: "sdnq",
    allocator: "torch",
    cap: Cap::Fraction { limit: MEMORY_FRACTION },
    // The fraction, and the placement. The two behaviours `sdcpp` requires are
    // *not* here and the module docs give the measurement behind each: tiling
    // seams the working resolution a small crop is upscaled into, and the
    // encoder's eviction is what the placement already decides rather than a
    // second promise. What stands behind these two is the parent's per-region
    // peak check, as it does for every backend.
    required: &[MEMORY_FRACTION, DEVICE_PLACEMENT],
};

pub const SDCPP: Contract = Contract {
    id: "sdcpp",
    allocator: "ggml",
    cap: Cap::Parent,
    // No allocator setter exists, so neither is required - requiring a name
    // nothing can answer to would fail every open, and inventing one would be
    // the reference's error again. What is required is the two behaviours ggml
    // does have a route to: `--vae-tiling` bounds the decode spike, and
    // `--clip-on-cpu` keeps the text encoder off the accelerator's allocator.
    required: &[VAE_TILING, TEXT_ENCODER_EVICTION],
};

impl Backend {
    pub fn contract(self) -> &'static Contract {
        match self {
            Backend::Mflux => &MFLUX,
            Backend::Sdnq => &SDNQ,
            Backend::Sdcpp => &SDCPP,
        }
    }

    pub fn id(self) -> &'static str {
        self.contract().id
    }

    /// A wire name to a backend. `None` for anything else, and `None` is
    /// refused rather than defaulted - rule 9 again: a backend this build has
    /// no contract for is a backend it cannot bound.
    pub fn from_id(id: &str) -> Option<Backend> {
        match id {
            "mflux" => Some(Backend::Mflux),
            "sdnq" => Some(Backend::Sdnq),
            "sdcpp" => Some(Backend::Sdcpp),
            _ => None,
        }
    }

    /// Whether **this build, on this platform** has a route to a render through
    /// this backend at all.
    ///
    /// A compile-time fact and not a probe, for the reason
    /// [`super::hardware`]'s old `HAS_BACKEND` gave: MLX is not something a
    /// machine acquires at runtime. What *is* a runtime question - whether the
    /// user's virtual environment holds this backend's Python packages - is
    /// answered by the sidecar itself and reported on `GET /v1/health`
    /// ([`super::wire::Health::backends`]), because the parent finds an install
    /// by looking for a `pyvenv.cfg` and cannot see inside it.
    ///
    /// - `mflux` is MLX, so macOS on Apple Silicon and nothing else.
    /// - `sdnq` is torch, which builds for every platform this application does.
    ///   Whether the *machine* has an accelerator is again the sidecar's to
    ///   answer, and it answers `501` at open when it does not.
    /// - `sdcpp` is `false` **everywhere**, because it renders nowhere: the
    ///   Python side declines every open. That is a statement about the
    ///   build rather than the platform, and it is spelled here so no caller has
    ///   to special-case a stub.
    pub const fn has_render_path(self) -> bool {
        match self {
            Backend::Mflux => cfg!(all(target_os = "macos", target_arch = "aarch64")),
            Backend::Sdnq => true,
            Backend::Sdcpp => false,
        }
    }

    /// Every backend, for the tests and for a settings panel that has to list
    /// them.
    pub const ALL: [Backend; 3] = [Backend::Mflux, Backend::Sdnq, Backend::Sdcpp];

    /// Every backend this build could actually render through here, in the order
    /// [`Backend::default_for_platform`] prefers them.
    pub fn offered() -> Vec<Backend> {
        Backend::ALL.into_iter().filter(|b| b.has_render_path()).collect()
    }

    /// What `Auto` means: **`mflux` on Apple Silicon, `sdnq` everywhere else.**
    ///
    /// Not a claim that one is better in general - it is that MLX is the runtime
    /// written for unified memory on the machine this repository's sweeps were
    /// measured on, and that
    /// torch is the only one of the two that exists anywhere else. A user who
    /// disagrees on either platform overrides it in Settings; that is the whole
    /// reason the setting exists rather than this function being the answer.
    pub fn default_for_platform() -> Option<Backend> {
        if Backend::Mflux.has_render_path() {
            return Some(Backend::Mflux);
        }
        Backend::offered().first().copied()
    }

    /// A setting's value to a backend: `"auto"`, `""` or an unknown string means
    /// [`Backend::default_for_platform`].
    ///
    /// `None` only where *nothing* renders here, which no shipped build is -
    /// `sdnq` renders on every platform - and is still expressed as a question
    /// over the table rather than as `true`, so removing a backend changes the
    /// answer instead of leaving a stale constant behind.
    pub fn from_setting(setting: Option<&str>) -> Option<Backend> {
        match setting.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("auto") => Backend::default_for_platform(),
            Some(id) => Backend::from_id(id).or_else(Backend::default_for_platform),
        }
    }
}

impl Contract {
    /// Whether the sidecar reported every guard this contract requires.
    ///
    /// The whole of the echo's value is here. Each of the reference's four
    /// guards was
    /// *present in the source* of the reference implementation and *absent from
    /// the inference path*, and no amount of reading the caller would have
    /// found that. Asking the process that ran to name what it applied is the
    /// only check that could.
    pub fn satisfied_by(&self, applied: &[String]) -> bool {
        self.missing_from(applied).is_empty()
    }

    /// Which required guards the sidecar did not report, so the log names them
    /// rather than saying the open failed.
    pub fn missing_from(&self, applied: &[String]) -> Vec<&'static str> {
        self.required
            .iter()
            .copied()
            .filter(|guard| !applied.iter().any(|applied| applied == guard))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_backend_names_its_allocator_and_round_trips_its_id() {
        for backend in Backend::ALL {
            let contract = backend.contract();
            assert!(!contract.allocator.is_empty(), "{backend:?} does not name an allocator");
            assert_eq!(Backend::from_id(contract.id), Some(backend));
        }
        assert_eq!(Backend::from_id("diffusers"), None, "an unknown backend is refused");
    }

    /// **The guard that names no backend does not exist here.** A cap by
    /// setter must name both setters; a cap by the parent must name neither,
    /// because a name nothing can answer to fails every open.
    #[test]
    fn a_cap_by_setter_names_its_setters_and_a_cap_by_the_parent_names_none() {
        for backend in Backend::ALL {
            let contract = backend.contract();
            match contract.cap {
                Cap::Allocator { limit, cache } => {
                    assert!(contract.required.contains(&limit), "{backend:?} does not require {limit}");
                    assert!(contract.required.contains(&cache), "{backend:?} does not require {cache}");
                    assert_ne!(limit, cache);
                }
                // One setter, and it must be required - the whole difference
                // from `Cap::Allocator` is that the *cache* has no knob, not
                // that the total does.
                Cap::Fraction { limit } => {
                    assert!(contract.required.contains(&limit), "{backend:?} does not require {limit}");
                    assert!(
                        !contract.required.iter().any(|g| g.contains("cache")),
                        "{backend:?} requires a cache knob torch does not have"
                    );
                }
                Cap::Parent => {
                    assert!(
                        !contract.required.iter().any(|g| g.contains("limit")),
                        "{backend:?} requires a limit setter it has no allocator for"
                    );
                }
            }
        }
    }

    /// `sdnq`'s two guards, and the two it deliberately does not carry. Its
    /// fraction bounds reserved memory - cache included - so a cache name would
    /// be a name nothing can answer to, and the two *behaviours* are either
    /// harmful here (tiling seams the working resolution) or already inside the
    /// placement (the encoder's eviction).
    #[test]
    fn sdnq_caps_the_fraction_and_places_and_promises_nothing_else() {
        let contract = Backend::Sdnq.contract();
        assert_eq!(contract.allocator, "torch");
        assert_eq!(contract.required, &[MEMORY_FRACTION, DEVICE_PLACEMENT]);
        assert!(!contract.required.contains(&VAE_TILING));
        assert!(!contract.required.contains(&TEXT_ENCODER_EVICTION));
        assert!(matches!(contract.cap, Cap::Fraction { limit } if limit == MEMORY_FRACTION));
    }

    /// **Which backends this build can render through, and what `Auto` picks.**
    ///
    /// The whole of the platform answer, and it is asserted as a *derivation
    /// from the table* rather than as a constant: `sdnq` renders everywhere,
    /// `mflux` only on Apple Silicon, `sdcpp` nowhere at all - so there is no
    /// longer a platform with no backend, and `Auto` still differs between
    /// platforms.
    #[test]
    fn auto_is_mflux_on_apple_silicon_and_sdnq_everywhere_else() {
        assert!(Backend::Sdnq.has_render_path(), "torch builds for every platform");
        assert!(!Backend::Sdcpp.has_render_path(), "the ggml stub renders nowhere");
        assert_eq!(
            Backend::Mflux.has_render_path(),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );

        let offered = Backend::offered();
        assert!(offered.contains(&Backend::Sdnq));
        assert!(!offered.contains(&Backend::Sdcpp));
        assert!(!offered.is_empty(), "every platform has at least the portable arm");

        let expected = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Backend::Mflux
        } else {
            Backend::Sdnq
        };
        assert_eq!(Backend::default_for_platform(), Some(expected));

        // The setting: absent, empty and `auto` are one answer; a named backend
        // is honoured even where it cannot render, because a refusal that names
        // the user's own choice is better than a silent substitution.
        assert_eq!(Backend::from_setting(None), Some(expected));
        assert_eq!(Backend::from_setting(Some("")), Some(expected));
        assert_eq!(Backend::from_setting(Some(" auto ")), Some(expected));
        assert_eq!(Backend::from_setting(Some("sdnq")), Some(Backend::Sdnq));
        assert_eq!(Backend::from_setting(Some("mflux")), Some(Backend::Mflux));
        assert_eq!(Backend::from_setting(Some("sdcpp")), Some(Backend::Sdcpp));
        // And a value nothing answers to falls back rather than failing: a
        // stale settings file must not make the rung unreachable.
        assert_eq!(Backend::from_setting(Some("diffusers")), Some(expected));
    }

    /// The two behaviours are required of `stable-diffusion.cpp`, which has a
    /// route to each and can hold it for every render.
    #[test]
    fn sdcpp_must_tile_the_vae_and_keep_the_text_encoder_off_the_accelerator() {
        let required = Backend::Sdcpp.contract().required;
        assert!(required.contains(&VAE_TILING));
        assert!(required.contains(&TEXT_ENCODER_EVICTION));
    }

    /// **And `mflux` requires neither, on purpose.** Evicting the encoder makes
    /// the second render through one open throw, and tiling the VAE seams
    /// the working resolution a small crop is upscaled to, so neither is a
    /// promise this backend can keep for every render - and the module's rule is
    /// that a promise it cannot keep is one it must not be asked for.
    #[test]
    fn mflux_requires_neither_behaviour_and_is_bounded_by_its_setters_and_the_parent() {
        let required = Backend::Mflux.contract().required;
        assert!(!required.contains(&VAE_TILING));
        assert!(!required.contains(&TEXT_ENCODER_EVICTION));
        assert_eq!(required, &[MX_MEMORY_LIMIT, MX_CACHE_LIMIT]);
    }

    #[test]
    fn an_open_that_did_not_apply_a_required_guard_is_not_satisfied() {
        let contract = Backend::Mflux.contract();
        let all: Vec<String> = contract.required.iter().map(|g| (*g).to_owned()).collect();
        assert!(contract.satisfied_by(&all));

        // Exactly the reference's shape: three of the four applied, the fourth
        // silently absent. `mx.set_cache_limit` is the one with no
        // caller.
        let short: Vec<String> =
            all.iter().filter(|g| *g != MX_CACHE_LIMIT).cloned().collect();
        assert!(!contract.satisfied_by(&short));
        assert_eq!(contract.missing_from(&short), vec![MX_CACHE_LIMIT]);

        // And a sidecar naming a guard that belongs to another backend does not
        // fill the hole. `PYTORCH_MPS_HIGH_WATERMARK_RATIO` is the reference's
        // own: correct for torch, and torch is not in this inference path.
        let wrong = vec!["PYTORCH_MPS_HIGH_WATERMARK_RATIO".to_owned()];
        assert_eq!(contract.missing_from(&wrong).len(), contract.required.len());
    }
}
