//! Which hardware runs a model, and who decides.
//!
//! Nothing in this application is CPU-only by design. Every session is built
//! through [`open_session`], every session gets an accelerator chosen for the
//! model it is about to run, and the user can override the choice per install.
//! What the accelerator *is* depends on the machine: CoreML on Apple silicon,
//! DirectML on any Direct3D 12 GPU (so NVIDIA, AMD and Intel alike), CUDA or
//! TensorRT where they are installed, WebGPU as a cross-vendor fallback.
//!
//! ## Why the inpainter still defaults to the CPU
//!
//! One operator decides it. LaMa's fast Fourier convolutions need `DFT`, and in
//! ONNX Runtime **`DFT` is implemented by three providers only** -
//! `core/providers/cpu/signal/dft.cc`,
//! `dml/DmlExecutionProvider/src/Operators/DmlDFT.h` and
//! `webgpu/math/dft.cc`. There is no CUDA, ROCm, CoreML, OpenVINO or TensorRT
//! implementation.
//!
//! When a provider cannot run an operator, ONNX Runtime does not fail: it
//! partitions the graph, gives the provider what it can take, and keeps the
//! rest on the CPU - with a tensor copy at every boundary. LaMa's FFT blocks
//! sit in the middle of the network, so the partition is not a tail that can be
//! sliced off; it is a dozen round trips. Measured: CoreML runs
//! LaMa at **3197 ms against 2719 ms on the CPU alone**, after a 23.7 s session
//! build. That is the mechanism, and it applies to CUDA for the same reason.
//!
//! **DirectML and WebGPU are the exceptions.** Both implement `DFT`, so on
//! either the whole graph is resident and there is no partition to pay for.
//! Which of them a machine gets is a packaging question, not a preference -
//! see [`crate::runtime::package`]: the macOS build carries WebGPU, the Windows
//! build carries DirectML, and **the WebGPU plugin rides beside every Windows
//! and Linux x64 runtime**, so every platform but Linux aarch64 has a
//! `DFT`-capable GPU provider whatever its vendor.
//!
//! ## Windows: one GPU provider for all three vendors, and CUDA beside it
//!
//! DirectML is Direct3D 12, so **NVIDIA, AMD and Intel all reach it through the
//! same download** - there is no per-vendor package to pick and no *vendor*
//! runtime for the user to install. It also has the `DFT` kernel, so the
//! inpainter is on the GPU there exactly as it is on a Mac. That is why
//! [`crate::runtime::package::Flavour::DirectMl`] is the Windows default and why
//! this module needs nothing vendor-specific to make a Windows machine as busy
//! as an Apple one.
//!
//! **There is one user-side install after all, and it is not a vendor's.**
//! Parsing the PE import directory of the shipped
//! `runtimes/packages/.dml/runtimes/win-x64/native/onnxruntime.dll`, its regular
//! imports include `MSVCP140.dll`, `MSVCP140_1.dll`, `VCRUNTIME140.dll` and
//! `VCRUNTIME140_1.dll` - the Visual C++ 2015-2022 Redistributable, which no
//! package [`crate::runtime::package`] downloads carries. Regular imports, so a
//! machine without it fails at load rather than at the first inference. That is
//! a dependency of the *runtime binary*, not of DirectML and not of a vendor:
//! the CUDA flavour has the same one. So the sentence above is about vendors
//! and stops there - what is true is that nothing **per vendor** has to be
//! installed for the GPU path, and the one thing that does have to be installed
//! is named with its own remedy rather than reported as a broken download, by
//! [`crate::runtime::LoadError::MissingDependency`]. Whether a Windows machine
//! that lacks it is common is not something this repository can say; it has
//! never run there.
//!
//! CUDA is offered as a **second flavour and never as the default**, and the
//! reason is this file's own rule rather than a preference. The two Windows
//! builds are different `onnxruntime.dll`s and the process loads exactly one, so
//! taking CUDA means giving up DirectML - and CUDA has no `DFT` kernel, so
//! [`Preference::Automatic`] never puts LaMa on it. What keeps the inpainter on
//! the GPU on that flavour is the WebGPU plugin unpacked beside it: an NVIDIA
//! user on CUDA gets the detector on CUDA and LaMa on WebGPU. Linux x64 has
//! the same two CUDA flavours on the same terms.
//!
//! ## Every figure below is an Apple M5's, and the other platforms are told so
//!
//! [`ModelProfile::measured_better`] is what was timed, and it was timed on
//! one machine, [`MEASURED_ON`]. WebGPU is in that list because it won on
//! Metal; the same provider on Windows is Direct3D 12 and on Linux is Vulkan,
//! through the plugin, and nobody has timed either. So [`choose_on`] - the
//! form every caller with a machine in reach uses - does two things off the
//! measured platform: it reports every automatic GPU choice as
//! `accel.chosen.unmeasured`, and it tries the platform's own
//! [`ModelProfile::unmeasured_candidates`] **before** the Mac's measured
//! winners. The second rule is what makes a downloaded flavour mean
//! something: on the DirectML flavour the detector and the inpainter both
//! land on DirectML, on a CUDA flavour the detector lands on CUDA, and WebGPU
//! takes whatever the platform's candidates could not - which on every CUDA
//! flavour is LaMa. A Mac reads the table exactly as before.
//!
//! CUDA also needs components this application does not ship - the CUDA runtime
//! and cuDNN 9 - which no other provider here does. [`Accelerator::availability`]
//! answers `Unavailable::MissingDependency` when they are not on the machine, so
//! a user who selects CUDA without installing it is told which of the two things
//! is missing rather than watching a session fail.
//!
//! **None of the Windows or Linux rows below were measured on that hardware.**
//! This repository has never executed there. DirectML and CUDA are
//! [`ModelProfile::unmeasured_candidates`] for that reason, reported as
//! `accel.chosen.unmeasured`, and neither has a
//! [`ModelProfile::measured_peak_rss`] row - so the memory gate does not fire on
//! either, by the rule three paragraphs down that says it gates on a measurement
//! or it does not gate.
//!
//! Measured on an M5, where the available one is WebGPU:
//!
//! | model | CPU | CoreML | WebGPU |
//! |---|---|---|---|
//! | manga-LaMa 512² | 2540 ms | 3899 ms | **1311 ms** |
//! | detector 1024² | 703 ms | **383 ms** | 548 ms |
//! | balloon 640² int8 | **129 ms** | *fails to build* | 222 ms |
//! | script-ID 48×200 | **2 ms** | - | 8 ms |
//!
//! So the inpainter is not CPU-bound after all - it is 1.9× faster on the GPU
//! than the best CPU thread count, through the one provider that has the
//! operator. And **no single provider wins**: the detector belongs on CoreML,
//! the inpainter on WebGPU, and the small models on the CPU, where the
//! transfer costs more than the compute. That is why the choice is per model
//! and why every entry below cites a measurement.
//!
//! ## Sessions are held, and regions are not batched
//!
//! Two more measurements, on the same machine, LaMa on WebGPU:
//!
//! - **Building the session costs 4.0 s and running it costs 1.47 s.** Building
//!   per region would nearly quadruple the work, so a session is opened once
//!   and held for the life of the job. Over 40 sequential runs the latency
//!   drifts +10.5%, which is worth watching and is not a leak.
//! - **Batching regions buys nothing.** manga-LaMa's only dynamic axis is
//!   `batch` - the spatial input is fixed at 512² - and per-region cost is flat
//!   from batch 1 to 4 and 29% *worse* at batch 8. A 512² LaMa saturates this
//!   GPU at batch 1. There is no batching path to build, which is the useful
//!   half of the result.
//!
//! ## A forced provider is honoured on speed, and gated on memory
//!
//! The standing ruling is that *"a forced
//! provider is honoured even when it is the slow choice, with the reason
//! reported"*, and [`Preference::Force`] below still says so. That ruling is
//! about **speed**, and the two figures that made this a question are not:
//!
//! | model | provider | peak process RSS | median |
//! |---|---|---|---|
//! | manga-LaMa | WebGPU | 740 MB | 986 ms |
//! | manga-LaMa | **CoreML** | **8.19 GB** | 1856 ms |
//!
//! It is not reachable automatically - [`Accelerator::implements_dft`] keeps
//! LaMa off CoreML - but the provider override is a per-install setting, so a
//! user can ask for it. An 8 GB working set is not a slow setting. On a machine
//! that cannot carry it, it is the failure the memory rule names: *"a runaway
//! must fail fast, into the ladder … rather than being absorbed into the memory
//! compressor, which loses the machine."*
//!
//! **So the third option was taken, and not the other two.** Not honour-and-warn,
//! because a warning is not a remedy and the thing being warned about is the
//! machine rather than the run. Not refuse-outright, because that would overturn
//! a standing ruling on the strength of a figure that is fine on the machine it
//! was measured on - 8.19 GB of a 32 GB Mac is inside what the platform grants,
//! and refusing it there would be this file inventing a policy the documents do
//! not state. What is implemented is the same arithmetic used elsewhere,
//! applied one rung down from the rung it was written for: a forced
//! provider whose **measured** peak fits what the platform will actually grant
//! is honoured, and one that does not is declined to the CPU under
//! `accel.declined.memory`, with the figure carried on the [`Declined`] so the
//! interface can say how much it would have needed.
//!
//! Three properties of that, each deliberate:
//!
//! - **It gates on a measurement or it does not gate.** The bound is
//!   [`ModelProfile::measured_peak_rss`], and a provider nobody has measured has
//!   no row there and is honoured. A guess that declines a user's setting is
//!   worse than no guess at all.
//! - **It gates on what the platform grants, not on physical RAM**, which is
//!   rule 9's fourth obligation and [`crate::memory`]'s whole subject.
//! - **[`Preference::CpuOnly`] is untouched.** It returns before any of this,
//!   because it is absolute and the golden tests pin it.
//!
//! ## What is measured, and what is not
//!
//! Every default below is marked. A number measured on one machine written down
//! as though it were general is the mistake made for the FLUX decision, and
//! this module is exactly where it would be made again - the table above is
//! an Apple M5, and a Windows machine with a discrete GPU will order these
//! differently. `spike-onnx-probe --ep <name>` is what produces it.
//!
//! Two named gaps, both about DirectML's device check
//! ([`Accelerator::publishes_devices`], [`device_present`]):
//!
//! - **That DirectML publishes devices at all is read, not run.** The evidence
//!   is the `DmlEpFactory` internal execution-provider factory in the shipped
//!   Windows binary, plus the measured fact that on the macOS runtime the
//!   equivalent factories - `CpuEpFactory` and `WebGpuEpFactory` - are exactly
//!   the two providers `Environment::devices` reports on this M5. Nobody has
//!   watched `GetEpDevices` return a DirectML device.
//! - **That a machine with no Direct3D 12 adapter yields an empty DirectML
//!   device list has never been observed.** It is the behaviour the check
//!   relies on, and the direction it fails in is the mild one: a runtime that
//!   lists a device DirectML cannot really use leaves the old behaviour intact,
//!   which is a failed session build and a fall back to the CPU. What would be
//!   costly is the other direction - a working Direct3D 12 machine whose
//!   adapter is missing from the list - and the guard in [`device_present`] is
//!   there for it: a device list with no CPU device in it is treated as a list
//!   this process could not read, and declines nothing.
//!
//! Neither can be closed from here. Both close the first time this is run on a
//! Windows machine with `spike-onnx-probe`, which is where the row for
//! DirectML in the table above comes from too.

use std::path::{Path, PathBuf};

use ort::session::Session;

use crate::memory;
use crate::runtime::package::{Os, Platform};
use crate::runtime::{self, Plugin};

/// The providers this build can register. Whether one is *usable* is a property
/// of the ONNX Runtime binary loaded at runtime, not of this list -
/// [`Accelerator::is_available`] asks the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Accelerator {
    Cpu,
    /// Apple silicon and recent Intel Macs.
    CoreMl,
    /// Any Direct3D 12 GPU on Windows: NVIDIA, AMD, Intel, Qualcomm.
    DirectMl,
    /// NVIDIA, via CUDA.
    Cuda,
    /// NVIDIA, via TensorRT. Faster than CUDA where it applies and much slower
    /// to build a session.
    TensorRt,
    /// AMD, via ROCm. Linux only in practice.
    Rocm,
    /// Intel CPUs, GPUs and NPUs.
    OpenVino,
    /// Cross-vendor and cross-platform, and one of the three providers that
    /// implements `DFT`.
    WebGpu,
    /// CPU, but with hand-written kernels that are often faster than the
    /// default ones on ARM.
    Xnnpack,
}

impl Accelerator {
    /// ONNX Runtime's own identifier, which is what `GetAvailableProviders`
    /// returns.
    pub fn ort_name(self) -> &'static str {
        match self {
            Accelerator::Cpu => "CPUExecutionProvider",
            Accelerator::CoreMl => "CoreMLExecutionProvider",
            Accelerator::DirectMl => "DmlExecutionProvider",
            Accelerator::Cuda => "CUDAExecutionProvider",
            Accelerator::TensorRt => "TensorrtExecutionProvider",
            Accelerator::Rocm => "ROCMExecutionProvider",
            Accelerator::OpenVino => "OpenVINOExecutionProvider",
            Accelerator::WebGpu => "WebGpuExecutionProvider",
            Accelerator::Xnnpack => "XnnpackExecutionProvider",
        }
    }

    /// The i18n key the interface names it under. No English crosses the seam.
    pub fn label_key(self) -> &'static str {
        match self {
            Accelerator::Cpu => "accel.cpu",
            Accelerator::CoreMl => "accel.coreml",
            Accelerator::DirectMl => "accel.directml",
            Accelerator::Cuda => "accel.cuda",
            Accelerator::TensorRt => "accel.tensorrt",
            Accelerator::Rocm => "accel.rocm",
            Accelerator::OpenVino => "accel.openvino",
            Accelerator::WebGpu => "accel.webgpu",
            Accelerator::Xnnpack => "accel.xnnpack",
        }
    }

    /// Whether this provider has a `DFT` kernel.
    ///
    /// Read off ONNX Runtime's own source tree, not inferred: `dft` appears
    /// under `providers/cpu`, `providers/dml` and `providers/webgpu`, and
    /// nowhere else. A model containing `DFT` on any other provider is a
    /// partitioned graph with a CPU island in the middle of it.
    pub fn implements_dft(self) -> bool {
        matches!(self, Accelerator::Cpu | Accelerator::DirectMl | Accelerator::WebGpu)
    }

    /// Whether this provider needs something the **user** installs, which this
    /// application neither ships nor can download.
    ///
    /// One family does: CUDA and TensorRT are built against the CUDA runtime
    /// and cuDNN, and the ONNX Runtime CUDA packages bundle neither - verified
    /// by listing them. Every other provider here is either in the
    /// operating system (DirectML, CoreML) or inside the runtime itself
    /// (WebGPU, XNNPACK).
    ///
    /// ROCm and OpenVINO also want a vendor runtime and are **not** listed,
    /// deliberately: nothing here downloads a package that carries them, so a
    /// machine that reports one has had it installed by hand and this module has
    /// no business second-guessing it. A check that gets it wrong takes a
    /// working provider away; the two below are checked because we offer a
    /// download for them.
    pub fn needs_cuda_runtime(self) -> bool {
        matches!(self, Accelerator::Cuda | Accelerator::TensorRt)
    }

    /// Whether this provider can be used here, and why not when it cannot.
    ///
    /// Three different absences, because the remedies are different: a
    /// provider that is not in the loaded build needs a different
    /// **download**, one whose vendor runtime is missing needs an
    /// **install**, and one that is loaded and found no device needs a
    /// **driver**. Telling a user with the CUDA build and no CUDA toolkit that
    /// CUDA is "not available in this runtime" would send them to fix the one
    /// thing that is already right.
    pub fn availability(self) -> Result<(), Unavailable> {
        if !self.in_runtime() {
            return Err(Unavailable::NotInRuntime);
        }
        if self.needs_cuda_runtime() && !cuda_runtime_present() {
            return Err(Unavailable::MissingDependency);
        }
        if self.needs_cuda_runtime() && !cuda_device_present() {
            return Err(Unavailable::NoDevice);
        }
        if self.publishes_devices() && !device_present(self) {
            return Err(Unavailable::NoDevice);
        }
        Ok(())
    }

    /// Whether the loaded runtime publishes an `OrtEpDevice` for this provider,
    /// which is what makes [`device_present`] a question worth asking about it
    /// at all.
    ///
    /// Two providers do. **A registered plugin does by construction** - it
    /// reaches the process through that same API, and there is no other way to
    /// hand it to a session ([`Dispatch::Device`]).
    ///
    /// **DirectML does because the shipped Windows binary carries an internal
    /// factory for it.** Read off
    /// `runtimes/packages/.dml/runtimes/win-x64/native/onnxruntime.dll`: the
    /// internal execution-provider factory classes named in its RTTI are
    /// `CpuEpFactory` and `DmlEpFactory`, and an internal factory is what
    /// publishes devices through `GetEpDevices`. `ort` adds nothing of its own
    /// on top - `Environment::devices` is a forward to that call
    /// (`ort/src/environment.rs:240-249`) - and its own documentation for
    /// `Device::ep_vendor` gives `"Microsoft"` for DirectML devices as the
    /// example, which is that list being described by the crate that reads it.
    ///
    /// The same reading on the macOS runtime this repository measures on names
    /// `CpuEpFactory` and `WebGpuEpFactory` and no CoreML one, and the device
    /// list on this M5 is exactly `CPUExecutionProvider` and
    /// `WebGpuExecutionProvider` - CoreML is absent from it although
    /// [`Accelerator::in_runtime`] answers `true` for CoreML on that build. So
    /// this is a list of the providers that *publish devices*, not a list of the
    /// providers that *work*, and asking it about CoreML would take the
    /// measured detector winner away from every Mac.
    fn publishes_devices(self) -> bool {
        self == Accelerator::WebGpu || self == Accelerator::DirectMl
    }

    /// Whether this provider reaches the process as a **plugin library**
    /// registered by [`crate::runtime::load`] rather than as part of the
    /// runtime's own build. One does today: WebGPU on Windows and Linux. It
    /// matters twice - the runtime does not list a plugin under
    /// `GetAvailableProviders`, and a session takes it through its
    /// **device** rather than through a provider-options call.
    fn is_plugin(self) -> bool {
        self == Accelerator::WebGpu && runtime::webgpu_plugin() == Plugin::Registered
    }

    /// Whether this provider can be used here at all. See [`Self::availability`]
    /// for which of the two absences it is.
    ///
    /// Compiled-in is not the same as usable - a CUDA build still fails at
    /// session creation without the CUDA runtime installed - so a caller that
    /// cares must handle [`open_session`] returning an error and fall back.
    pub fn is_available(self) -> bool {
        self.availability().is_ok()
    }

    /// Present in the loaded runtime, before vendor libraries and hardware are checked.
    pub fn is_installed(self) -> bool {
        self.in_runtime()
    }

    /// Whether the **loaded** ONNX Runtime was built with this provider. The
    /// binary's own claim about itself, and half of [`Self::availability`].
    fn in_runtime(self) -> bool {
        use ort::ep::ExecutionProvider;
        match self {
            Accelerator::Cpu => true,
            Accelerator::CoreMl => ort::ep::coreml::CoreML::default().is_available().unwrap_or(false),
            Accelerator::DirectMl => ort::ep::directml::DirectML::default().is_available().unwrap_or(false),
            Accelerator::Cuda => ort::ep::cuda::CUDA::default().is_available().unwrap_or(false),
            Accelerator::TensorRt => ort::ep::tensorrt::TensorRT::default().is_available().unwrap_or(false),
            Accelerator::Rocm => ort::ep::rocm::ROCm::default().is_available().unwrap_or(false),
            Accelerator::OpenVino => ort::ep::openvino::OpenVINO::default().is_available().unwrap_or(false),
            // Built into the macOS archive, a registered plugin everywhere
            // else that has it. Either way it is one provider of one name.
            Accelerator::WebGpu => {
                ort::ep::webgpu::WebGPU::default().is_available().unwrap_or(false) || self.is_plugin()
            }
            Accelerator::Xnnpack => ort::ep::xnnpack::XNNPACK::default().is_available().unwrap_or(false),
        }
    }

    fn dispatch(self) -> Option<Dispatch> {
        if self.is_plugin() {
            return Some(Dispatch::Device(self.ort_name()));
        }
        Some(Dispatch::BuiltIn(match self {
            Accelerator::Cpu => return None,
            Accelerator::CoreMl => ort::ep::coreml::CoreML::default().build(),
            Accelerator::DirectMl => ort::ep::directml::DirectML::default().build(),
            Accelerator::Cuda => ort::ep::cuda::CUDA::default().build(),
            Accelerator::TensorRt => ort::ep::tensorrt::TensorRT::default().build(),
            Accelerator::Rocm => ort::ep::rocm::ROCm::default().build(),
            Accelerator::OpenVino => ort::ep::openvino::OpenVINO::default().build(),
            Accelerator::WebGpu => ort::ep::webgpu::WebGPU::default().build(),
            Accelerator::Xnnpack => ort::ep::xnnpack::XNNPACK::default().build(),
        }))
    }
}

/// How a session takes a provider. A provider compiled into the runtime is
/// appended by its options; a plugin has no options call and is appended by
/// the **devices** the runtime discovered for it, selected by provider name.
enum Dispatch {
    BuiltIn(ort::ep::ExecutionProviderDispatch),
    Device(&'static str),
}

/// Whether the runtime discovered a device for a provider that publishes them
/// ([`Accelerator::publishes_devices`]).
///
/// A provider is registered whether or not it finds hardware; it is the device
/// list that says. The WebGPU plugin with no adapter is a Linux machine without
/// the Vulkan loader or a Windows machine without a working GPU driver.
/// DirectML with no device is a machine with no Direct3D 12 adapter - an old
/// GPU, a virtual machine, a remote session - and before this asked, those
/// machines were routed to DirectML for both the detector and the inpainter and
/// found out by having each session build fail. Both are a remedy the user
/// applies to their machine rather than a download this application offers.
///
/// **A device list this process could not read denies nothing.** `ort` swallows
/// an error from `GetEpDevices` and hands back an empty iterator
/// (`ort/src/environment.rs:243-244`, on the grounds that a minimal build does
/// not support the call), so an empty list is two different answers - no
/// devices, or no answer - and only one of them is grounds for declining a
/// provider. Every shipped runtime read so far carries `CpuEpFactory`, and the
/// device list measured on this machine contains `CPUExecutionProvider`, so a
/// list with no CPU device in it is a list that was not produced. That case
/// answers `true` and lets the session build decide, which is the older
/// behaviour and the one this file's own rule prefers: a check that gets it
/// wrong takes a working provider away.
fn device_present(accelerator: Accelerator) -> bool {
    let Ok(env) = ort::environment::Environment::current() else {
        return accelerator != Accelerator::WebGpu;
    };
    if accelerator == Accelerator::WebGpu {
        // The built-in macOS EP publishes devices too. A provider name alone
        // is insufficient: a headless or broken adapter is not a GPU path.
        return env.devices().any(|device| {
            device.ep().ok() == Some(accelerator.ort_name())
                && device.hardware_device().ty() == ort::memory::DeviceType::GPU
        });
    }
    listed_or_unreadable(env.devices().filter_map(|device| device.ep().ok()), accelerator.ort_name())
}

/// The rule [`device_present`] applies to whatever the runtime listed. Split out
/// so the arithmetic can be tested without a loaded runtime, which is the half
/// of it that is not about hardware.
fn listed_or_unreadable<'a>(listed: impl IntoIterator<Item = &'a str>, wanted: &str) -> bool {
    let (mut found, mut cpu) = (false, false);
    for name in listed {
        found |= name == wanted;
        cpu |= name == Accelerator::Cpu.ort_name();
    }
    found || !cpu
}

/// Why a provider this build knows how to name cannot be used on this machine.
///
/// Both halves reach the interface, and they are separate because their
/// remedies are - see [`Accelerator::availability`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// The loaded ONNX Runtime was not built with it. A different download.
    NotInRuntime,
    /// It is in the runtime and the machine is missing what the *user* has to
    /// install for it - the CUDA runtime, cuDNN. An install, not a download.
    MissingDependency,
    /// It is registered and found no device to run on. A driver - on Linux,
    /// the Vulkan loader the WebGPU plugin opens adapters through; on Windows,
    /// a machine with no Direct3D 12 adapter for DirectML to sit on.
    NoDevice,
}

impl Unavailable {
    /// The i18n key the interface names it under. No English crosses the seam.
    pub fn reason_key(self) -> &'static str {
        match self {
            Unavailable::NotInRuntime => "accel.declined.unavailable",
            Unavailable::MissingDependency => "accel.declined.missingRuntime",
            Unavailable::NoDevice => "accel.declined.noDevice",
        }
    }
}

/// Every provider this build knows how to name, in the order the interface
/// lists them. Not a preference order - nothing routes on it; the routing is
/// [`ModelProfile`], per model.
pub const KNOWN: [Accelerator; 9] = [
    Accelerator::Cpu,
    Accelerator::CoreMl,
    Accelerator::DirectMl,
    Accelerator::Cuda,
    Accelerator::TensorRt,
    Accelerator::Rocm,
    Accelerator::OpenVino,
    Accelerator::WebGpu,
    Accelerator::Xnnpack,
];

/// Every provider the loaded runtime offers *and* this machine can actually
/// reach. Used to populate the setting and the diagnostics panel.
pub fn available() -> Vec<Accelerator> {
    KNOWN.into_iter().filter(|a| a.is_available()).collect()
}

/// Whether CUDA and cuDNN libraries this machine would need are installed.
///
/// Asked of the filesystem rather than of ONNX Runtime, because ONNX Runtime's
/// own answer is the wrong one: `GetAvailableProviders` lists
/// `CUDAExecutionProvider` because the build has it, and the first anyone hears
/// about a missing `cudart` is a session that fails to build several seconds
/// later. The cost of asking is one `read_dir` per candidate directory, once
/// per call, and only on a build that carries CUDA at all.
pub fn cuda_runtime_present() -> bool {
    cuda_runtime_in(&cuda_search_dirs())
}

/// Ask the NVIDIA driver for a real adapter. A CUDA EP compiled into ORT and
/// libraries on disk still do not establish that a GPU is connected.
fn cuda_device_present() -> bool {
    std::process::Command::new("nvidia-smi")
        .arg("--list-gpus")
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.windows(4).any(|part| part == b"GPU "))
}

/// Where a CUDA install puts its runtime library, on this machine.
///
/// The loader's own search path first - a working CUDA install is on it, by
/// construction, because that is how every other program finds it - then
/// `CUDA_PATH`, which NVIDIA's Windows installer sets, then the two fixed
/// places a Linux install lands.
fn cuda_search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    let loader_path = if cfg!(target_os = "windows") { "PATH" } else { "LD_LIBRARY_PATH" };
    if let Ok(value) = std::env::var(loader_path) {
        dirs.extend(std::env::split_paths(&value));
    }

    if let Ok(root) = std::env::var("CUDA_PATH") {
        let root = PathBuf::from(root);
        dirs.push(root.join(if cfg!(target_os = "windows") { "bin" } else { "lib64" }));
    }

    if cfg!(target_os = "linux") {
        dirs.push(PathBuf::from("/usr/lib/x86_64-linux-gnu"));
        dirs.push(PathBuf::from("/usr/lib64"));
        // `/usr/local/cuda` is the symlink a normal install leaves, and
        // `/usr/local/cuda-13.0` is what it points at. Both are listed because
        // a machine with two toolkits has only the versioned ones.
        if let Ok(entries) = std::fs::read_dir("/usr/local") {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("cuda") {
                    dirs.push(entry.path().join("lib64"));
                }
            }
        }
    }

    dirs
}

/// The same question, against a list of directories. Split out for
/// [`choose_within`]'s reason: the rule is testable without a CUDA install.
pub fn cuda_runtime_in(dirs: &[PathBuf]) -> bool {
    let mut cuda = false;
    let mut cudnn = false;
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                cuda |= is_cuda_runtime_library(&name);
                cudnn |= is_cudnn_runtime_library(&name);
            }
        }
    }
    cuda && cudnn
}

/// The shipped CUDA EP targets cuDNN 9. A cuDNN 8 install is a missing
/// dependency for this build even if the CUDA toolkit itself is present.
pub fn is_cudnn_runtime_library(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "cudnn64_9.dll" || name == "libcudnn.so.9" || name.starts_with("libcudnn.so.9.")
}

/// Whether one filename is the CUDA runtime library.
///
/// `cudart64_12.dll` and `cudart64_13.dll` on Windows, `libcudart.so.12` and
/// `libcudart.so.13` on Linux. The major version is deliberately not matched:
/// this answers "is CUDA installed at all", and a toolkit whose major does not
/// match the build fails at session creation with a message that says so, which
/// is a better answer than this function pretending to know which majors a
/// given `onnxruntime_providers_cuda.dll` accepts.
pub fn is_cuda_runtime_library(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    (name.starts_with("cudart64") && name.ends_with(".dll")) || name.starts_with("libcudart.so")
}

/// What the user asked for. `Automatic` is the default and is the only value
/// that consults the table below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preference {
    #[default]
    Automatic,
    /// Never leave the CPU. The setting that makes a run reproducible -
    /// the golden tests need it, and so does anyone comparing two machines.
    CpuOnly,
    /// Use this provider wherever the model allows it. **Not** silently
    /// downgraded: a forced provider that is missing is reported.
    Force(Accelerator),
}

/// What a model needs from a provider, and what has actually been measured for
/// it.
#[derive(Debug, Clone, Copy)]
pub struct ModelProfile {
    pub name: &'static str,
    /// The i18n key the interface names this model under - the same
    /// `models.kind.*` key [`crate::registry::Kind::label_key`] uses, so a
    /// settings panel listing "where each model runs" and the loaded-models tab
    /// name the same thing the same way. A key rather than a name because no
    /// English crosses the seam.
    pub label_key: &'static str,
    /// The graph contains `DFT`. Only [`Accelerator::implements_dft`]
    /// providers can hold the whole graph; every other one partitions.
    pub uses_dft: bool,
    /// Providers **measured** to beat the CPU for this model, best first. Empty
    /// means nothing has been measured. Measured on [`MEASURED_ON`] and
    /// nowhere else; [`choose_on`] is what says so to the other platforms.
    pub measured_better: &'static [Accelerator],
    /// Providers that are expected to help on mechanism alone but that nobody
    /// has timed for this model. `Automatic` reaches them **only** when nothing
    /// measured is present, and says so - `accel.chosen.unmeasured`.
    ///
    /// This exists because the alternative is worse. The Windows GPU package
    /// carries DirectML and nothing else, DirectML is one of the three
    /// providers with a `DFT` kernel, and leaving a machine on the CPU because
    /// the benchmark was run on a Mac would waste a download made for exactly
    /// this. A guess reported as a guess is not the error; a guess written
    /// down as a measurement is.
    pub unmeasured_candidates: &'static [Accelerator],
    /// Peak **process** RSS measured with this model held on a given provider,
    /// over a multi-region page - the figure Phase 3's memory criterion is
    /// written against.
    ///
    /// It is a whole-process number and not a session's, because that is what
    /// the budget is written in and because the part that dominates is the
    /// runtime's allocator arena, which is nobody's resident object. So the
    /// core's own sessions are already inside these figures and must not be
    /// added to them.
    ///
    /// A provider with no row here has not been measured for this model, and
    /// [`choose_within`] does not gate on a number nobody has.
    pub measured_peak_rss: &'static [(Accelerator, u64)],
    /// Providers this graph cannot be placed on whole, so a forced session on
    /// one of them keeps the CPU fallback for the nodes it leaves behind
    /// instead of failing strict placement. Every other forced provider must
    /// hold the whole graph or refuse.
    pub partitioned_on: &'static [Accelerator],
}

impl ModelProfile {
    /// What this model was measured to peak at on one provider, if anyone has
    /// measured it.
    pub fn peak_on(&self, accelerator: Accelerator) -> Option<u64> {
        self.measured_peak_rss
            .iter()
            .find(|(candidate, _)| *candidate == accelerator)
            .map(|(_, bytes)| *bytes)
    }
}

/// `comictextdetector.onnx`. CoreML was faster in a mixed CoreML/CPU run, but
/// strict graph placement proves it needs CPU nodes. WebGPU owns the whole
/// graph and is the automatic GPU choice on the measured Mac.
pub const DETECTOR: ModelProfile = ModelProfile {
    name: "detector",
    label_key: "models.kind.textDetector",
    uses_dft: false,
    measured_better: &[Accelerator::WebGpu],
    // Windows, in the order the two flavours are offered: DirectML is the
    // default download and reaches every vendor; CUDA is the opt-in one and is
    // only ever in this list because the detector has no `DFT` in it. Neither
    // has been timed.
    unmeasured_candidates: &[Accelerator::DirectMl, Accelerator::Cuda],
    // The detector is the one model `Automatic` sends to CoreML, and its peak
    // there is the budget itself - the ~195 MB session and the ~1.2 GB
    // arena - rather than a figure of its own. Nothing has measured it per
    // provider, so nothing is claimed.
    measured_peak_rss: &[],
    partitioned_on: &[],
};

/// `comic-text-and-bubble-detector`. Measured: CPU 129 ms, WebGPU 222 ms, and
/// **CoreML cannot build a session for it at all** - it is an int8 quantised
/// model. The failure costs 8 s to discover, which is why [`open_session`]'s
/// fallback is reported rather than retried per page.
pub const BALLOON: ModelProfile = ModelProfile {
    name: "balloon",
    label_key: "models.kind.balloonDetector",
    uses_dft: false,
    measured_better: &[],
    unmeasured_candidates: &[],
    measured_peak_rss: &[],
    partitioned_on: &[],
};

/// Full FP32 RT-DETR uses the same ONNX session builder as the small detector.
/// No GPU timing has been established, so Automatic remains on CPU. A forced
/// GPU must commit the entire graph there or fail.
pub const FULL_RT: ModelProfile = ModelProfile {
    name: "rt-full",
    label_key: "models.kind.balloonDetector",
    uses_dft: false,
    measured_better: &[],
    unmeasured_candidates: &[],
    measured_peak_rss: &[],
    partitioned_on: &[],
};

/// `osd_lstm.onnx`. Measured: CPU 2 ms, WebGPU 8 ms on a 48×200 strip. An LSTM
/// over a strip that small is dominated by the transfer, and the gate runs it
/// once per text line, so the CPU wins by four times.
pub const SCRIPT_ID: ModelProfile = ModelProfile {
    name: "script-id",
    label_key: "models.kind.scriptGate",
    uses_dft: false,
    measured_better: &[],
    unmeasured_candidates: &[],
    measured_peak_rss: &[],
    partitioned_on: &[],
};

/// `lama-manga.onnx`. The one with `DFT` in it.
///
/// Measured: WebGPU **1311 ms**, CPU 2540 ms at two threads, CoreML 3899 ms
/// after a 25.7 s session build. The ordering is the `DFT` rule made visible -
/// the provider that has the kernel is nearly twice the CPU, and the one that
/// does not is half of it.
///
/// DirectML also has the kernel and is unmeasured; on Windows it is the first
/// thing to try.
pub const LAMA: ModelProfile = ModelProfile {
    name: "lama",
    label_key: "models.kind.inpainter",
    uses_dft: true,
    measured_better: &[Accelerator::WebGpu],
    // The Windows GPU path, and **CUDA is deliberately not beside it**. It has
    // the `DFT` kernel, so the partition that made CoreML slower does not apply
    // - but nobody has timed it. `choose_within` would filter CUDA out of this
    // list anyway (`implements_dft`); leaving it out says the same thing where a
    // reader looks first.
    unmeasured_candidates: &[Accelerator::DirectMl],
    // Forty sequential regions through one held session, peak process RSS flat
    // from the first region to the fortieth in all three rows. CoreML is
    // eleven times WebGPU.
    measured_peak_rss: &[
        (Accelerator::WebGpu, 740 * memory::MIB),
        (Accelerator::Cpu, 1167 * memory::MIB),
        (Accelerator::CoreMl, 8387 * memory::MIB),
    ],
    // The 1311 ms above was WebGPU with the runtime's automatic placement,
    // which leaves the shape arithmetic on the CPU: 2808 of 8163 nodes
    // (`Concat`, `Gather`, `Slice`, `Cast` and the like), about 38 ms of a run.
    // Strict placement refuses that graph outright on WebGPU, and every other
    // GPU provider offered for it lacks at least the shape ops or `DFT`, so a
    // forced GPU provider keeps the fallback. CoreML is not offered
    // (`models::supports_model`), so it stays strict.
    partitioned_on: &[Accelerator::WebGpu, Accelerator::DirectMl, Accelerator::Cuda],
};

/// `manga-ocr-base`, the gate's rescue reader - a ViT encoder and a two-layer
/// BERT decoder, opened as two sessions and profiled as one model because a
/// reading is always both.
///
/// Measured on the machine [`MEASURED_ON`] names, one 224² crop, ten runs:
///
/// | | encoder | decoder, 8 tokens |
/// | --- | --- | --- |
/// | CPU | 270 ms | 4 ms |
/// | CoreML | 218 ms | 15 ms |
///
/// **CoreML builds both graphs** - it is not the balloon detector's case,
/// where an int8 model could not be built at all - and it is still not worth
/// taking. The encoder's 52 ms is bought for a **6.6 s session build**, and the
/// decoder is run once per token, where CoreML is nearly four times the CPU.
/// A rescue is a handful of regions on a page, so the session build is the
/// whole of the bill and it never amortises.
/// Nothing is in `measured_better`, which is how this table says *CPU*.
pub const OCR: ModelProfile = ModelProfile {
    name: "ocr",
    label_key: "models.kind.ocr",
    uses_dft: false,
    measured_better: &[],
    unmeasured_candidates: &[],
    measured_peak_rss: &[],
    partitioned_on: &[],
};

/// Hayai OCR v2.5 Nova, the text reader: a SigLIP2 vision graph and a
/// twelve-layer decoder run once per token, opened as two sessions and
/// profiled as one model, as [`OCR`] is.
///
/// Measured on the machine [`MEASURED_ON`] names, 64 detected regions from one
/// chapter, per region with the first read excluded:
///
/// | vision / decoder | FP32 | FP16 |
/// | --- | --- | --- |
/// | CPU / CPU | **190 to 230 ms** | 335 ms |
/// | CPU / WebGPU | 234 ms | 217 ms |
/// | CoreML / CPU | 618 ms | - |
/// | CoreML / CoreML | fails on the first step | 342 ms |
/// | WebGPU / any | fails on every read | fails |
///
/// So nothing is in `measured_better`. The decoder's token loop is small
/// operations, where a GPU pays a transfer per step and wins nothing. The
/// vision graph cannot run on WebGPU at all: its position-embedding `Resize`
/// is antialiased, which WebGPU does not implement, and the failure comes at
/// the first read, after the session built. Split out to the CPU, the rest of
/// the vision graph ran about 10 % faster on WebGPU, inside the run-to-run
/// spread. CoreML cannot take the decoder's empty vision input on a step.
///
/// **CUDA is the one GPU that helped**, and it was timed on a cloud NVIDIA L4
/// (Linux, onnxruntime-gpu 1.23.2), not here: 303 ms FP32 and 167 ms FP16
/// against 497 ms on the same container's four CPU cores. That is why it is an
/// unmeasured candidate rather than a measurement. The CUDA readings matched
/// the CPU's on 59 of 64 regions (floating-point order flips a few marginal
/// greedy steps). DirectML is untested and not offered: whether it implements
/// an antialiased `Resize` is not known here, and WebGPU shows the failure
/// would only appear mid-run. AMD and Intel GPUs therefore read on the CPU.
pub const HAYAI: ModelProfile = ModelProfile {
    name: "hayai",
    label_key: "models.kind.hayai",
    uses_dft: false,
    measured_better: &[],
    unmeasured_candidates: &[Accelerator::Cuda],
    measured_peak_rss: &[],
    // The CUDA run above kept the CPU provider beside it for the shape
    // arithmetic, so a forced CUDA session keeps the fallback too.
    partitioned_on: &[Accelerator::Cuda],
};

/// Every model this application routes, so that "where will each model run"
/// can be answered without a session and without naming them twice.
pub const PROFILES: [&ModelProfile; 6] = [&DETECTOR, &BALLOON, &SCRIPT_ID, &LAMA, &OCR, &HAYAI];

/// Why a session ended up where it did. Carried into the patch's provenance
/// as `execution_provider` and surfaced in diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub accelerator: Accelerator,
    /// Set when a forced provider could not be used. The interface says so
    /// rather than quietly running somewhere else.
    pub declined: Option<Declined>,
    /// Set when the choice was reasoned about rather than timed.
    pub note: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declined {
    pub wanted: Accelerator,
    pub reason_key: &'static str,
    /// What the declined provider was measured to need, where that is the
    /// reason it was declined. The figure is on the outcome rather than baked
    /// into the sentence, because the sentence is a catalogue key and the
    /// number is not translatable.
    pub needed_bytes: Option<u64>,
    /// What there was room for at the moment the choice was made.
    pub room_bytes: Option<u64>,
}

impl Declined {
    /// The plain shape: a refusal with no arithmetic behind it.
    fn plain(wanted: Accelerator, reason_key: &'static str) -> Declined {
        Declined { wanted, reason_key, needed_bytes: None, room_bytes: None }
    }
}

impl Selection {
    /// Whether this choice rests on a measurement for this model on this
    /// hardware. The interface shows the difference.
    pub fn is_measured(&self) -> bool {
        self.note.is_none()
    }
}

/// Pick a provider for one model, without a memory answer.
///
/// This is [`choose_within`] with `room` unknown, which is the reading a caller
/// that has no business asking the machine should get deliberately, and the one
/// any platform gets when its own probe fails. It stopped being "every platform
/// but macOS" once Windows and Linux got a probe of their own. It is the
/// whole of the selection logic apart from the memory gate, so every rule
/// about `DFT`, measured winners and unmeasured candidates is testable
/// without a machine.
pub fn choose(profile: &ModelProfile, preference: Preference, available: &[Accelerator]) -> Selection {
    choose_within(profile, preference, available, None)
}

/// The one machine every figure in this file was measured on: an Apple M5,
/// so macOS. Every other platform is unmeasured, and [`choose_on`] says so.
pub const MEASURED_ON: Os = Os::MacOs;

/// Pick a provider for one model, on a named platform.
///
/// [`choose_within`] reads the table as written, which is right on the
/// platform it was written on. Anywhere else, two things change, and only
/// under [`Preference::Automatic`] - a forced provider and `CpuOnly` are what
/// they are on every machine:
///
/// - **Every GPU choice is a guess and is reported as one**,
///   `accel.chosen.unmeasured`, however it was reached. WebGPU measured on
///   Metal is not WebGPU on Vulkan.
/// - **The platform's own candidates come before the Mac's winners.** A
///   candidate row exists for a provider that is not on a Mac at all, and it
///   was written for the machine that has it - so on the DirectML flavour
///   both GPU models go to DirectML, and on a CUDA flavour the detector goes to
///   CUDA rather than to the plugin beside it. WebGPU then takes what the
///   candidates could not, which is LaMa on every CUDA flavour.
///
/// `host` is `None` for a platform this build cannot name, which is treated
/// as unmeasured because it is.
pub fn choose_on(
    profile: &ModelProfile,
    preference: Preference,
    available: &[Accelerator],
    room: Option<u64>,
    host: Option<Platform>,
) -> Selection {
    if host.map(|platform| platform.os) == Some(MEASURED_ON) {
        return choose_within(profile, preference, available, room);
    }
    if preference != Preference::Automatic {
        return choose_within(profile, preference, available, room);
    }
    let candidate = profile
        .unmeasured_candidates
        .iter()
        .chain(profile.measured_better.iter())
        .copied()
        .find(|a| usable_automatically(profile, available, *a));
    match candidate {
        Some(accelerator) => Selection { accelerator, declined: None, note: Some("accel.chosen.unmeasured") },
        None => Selection { accelerator: Accelerator::Cpu, declined: None, note: None },
    }
}

/// [`Preference::Automatic`]'s one rule about a provider: it is present, and
/// it would not partition a `DFT` graph.
fn usable_automatically(profile: &ModelProfile, available: &[Accelerator], a: Accelerator) -> bool {
    (a == Accelerator::Cpu || available.contains(&a)) && (!profile.uses_dft || a.implements_dft())
}

/// Pick a provider for one model, against the room this machine has.
///
/// `room` is [`crate::memory::room`] - what a whole process may reach here -
/// and `None` means *nobody could say*, which is honoured rather than refused.
/// See the module docs for why the memory gate exists at all and why it is
/// arithmetic rather than a blanket refusal.
///
/// This reads the table as measured. A caller with a machine in reach wants
/// [`choose_on`], which knows the table was measured on one platform.
pub fn choose_within(
    profile: &ModelProfile,
    preference: Preference,
    available: &[Accelerator],
    room: Option<u64>,
) -> Selection {
    let has = |a: Accelerator| a == Accelerator::Cpu || available.contains(&a);

    match preference {
        // Absolute, and first: the golden tests pin it, and the CPU is the
        // one provider whose peak is the budget's own row rather than a
        // surprise.
        Preference::CpuOnly => {
            Selection { accelerator: Accelerator::Cpu, declined: None, note: None }
        }

        Preference::Force(wanted) => {
            if !has(wanted) {
                return Selection {
                    accelerator: Accelerator::Cpu,
                    declined: Some(Declined::plain(wanted, "accel.declined.unavailable")),
                    note: None,
                };
            }
            // **The memory gate**, and it fires before the `DFT` one because
            // its answer is different in kind: the partition costs seconds and
            // this costs the machine. It fires only where all three of a
            // measurement, a room figure and an overrun exist - a provider
            // nobody has measured, or a machine nobody can ask about, is
            // honoured exactly as it was before this existed.
            if let (Some(needed), Some(room)) = (profile.peak_on(wanted), room) {
                if needed > room {
                    return Selection {
                        accelerator: Accelerator::Cpu,
                        declined: Some(Declined {
                            wanted,
                            reason_key: "accel.declined.memory",
                            needed_bytes: Some(needed),
                            room_bytes: Some(room),
                        }),
                        note: None,
                    };
                }
            }
            if profile.uses_dft && !wanted.implements_dft() {
                // Honoured, and reported. The user asked for it and it will
                // work; it will also be slower than the CPU, and saying so is
                // the difference between a setting and a trap.
                return Selection {
                    accelerator: wanted,
                    declined: Some(Declined::plain(wanted, "accel.declined.partitioned")),
                    note: None,
                };
            }
            Selection { accelerator: wanted, declined: None, note: None }
        }

        Preference::Automatic => {
            // A provider that would partition a `DFT` graph is never chosen
            // automatically, whatever else is known about it.
            let usable = |a: &Accelerator| usable_automatically(profile, available, *a);

            if let Some(measured) = profile.measured_better.iter().copied().find(usable) {
                return Selection { accelerator: measured, declined: None, note: None };
            }
            if let Some(candidate) = profile.unmeasured_candidates.iter().copied().find(usable) {
                return Selection {
                    accelerator: candidate,
                    declined: None,
                    note: Some("accel.chosen.unmeasured"),
                };
            }
            Selection { accelerator: Accelerator::Cpu, declined: None, note: None }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("{model}: the session could not be built on {accelerator:?}: {detail}")]
    Build { model: &'static str, accelerator: Accelerator, detail: String },
}

/// Build a session for one model, on the chosen provider, and say where it
/// landed.
///
/// A provider that is compiled in can still fail at session creation - a CUDA
/// build without the CUDA runtime installed is the usual case - so a failure
/// on anything but the CPU retries on the CPU rather than taking the
/// application down. The retry is reported, never silent.
pub fn open_session(
    model: &Path,
    profile: &ModelProfile,
    preference: Preference,
    intra_threads: Option<usize>,
) -> Result<(Session, Selection), SessionError> {
    // The one call that consults the machine. Everything else in this module
    // takes the room and the platform as arguments, so the policy is testable
    // and only the session that is actually about to be built pays for a
    // sysctl.
    let mut selection =
        choose_on(profile, preference, &available(), memory::room(), Platform::host());

    // `choose_within` is pure: it is handed a list of what is present and can
    // only say "not in the list". The machine knows more than that - a build
    // that carries CUDA on a machine with no CUDA runtime is a different
    // sentence from a build without CUDA in it - so the finer reason is
    // attached here, where the machine is in reach.
    if let Some(declined) = selection.declined.as_mut() {
        if declined.reason_key == Unavailable::NotInRuntime.reason_key() {
            if let Err(reason) = declined.wanted.availability() {
                declined.reason_key = reason.reason_key();
            }
        }
    }

    if let Preference::Force(wanted) = preference {
        if let Some(declined) = selection.declined {
            if selection.accelerator != wanted {
                return Err(SessionError::Build {
                    model: profile.name,
                    accelerator: wanted,
                    detail: format!("requested backend is unavailable ({})", declined.reason_key),
                });
            }
        }
    }

    match build(model, selection.accelerator, intra_threads, strict_placement(profile, preference, selection.accelerator)) {
        Ok(session) => Ok((session, selection)),
        Err(detail) if selection.accelerator != Accelerator::Cpu && !matches!(preference, Preference::Force(_)) => {
            let wanted = selection.accelerator;
            selection = Selection {
                accelerator: Accelerator::Cpu,
                declined: Some(Declined::plain(wanted, "accel.declined.failed")),
                note: None,
            };
            let session = build(model, Accelerator::Cpu, intra_threads, false)
                .map_err(|detail| SessionError::Build { model: profile.name, accelerator: Accelerator::Cpu, detail })?;
            let _ = detail;
            Ok((session, selection))
        }
        Err(detail) => Err(SessionError::Build { model: profile.name, accelerator: selection.accelerator, detail }),
    }
}

/// Whether a session must hold the whole graph on its provider. A forced
/// provider must, unless the measurement this profile rests on was itself a
/// partition on that provider - [`ModelProfile::partitioned_on`].
fn strict_placement(profile: &ModelProfile, preference: Preference, accelerator: Accelerator) -> bool {
    matches!(preference, Preference::Force(_)) && !profile.partitioned_on.contains(&accelerator)
}

fn build(model: &Path, accelerator: Accelerator, intra_threads: Option<usize>, strict: bool) -> Result<Session, String> {
    let mut builder = Session::builder().map_err(|e| e.to_string())?;
    if let Some(threads) = intra_threads {
        builder = builder.with_intra_threads(threads).map_err(|e| e.to_string())?;
    }
    if strict && accelerator != Accelerator::Cpu {
        builder = builder.with_disable_cpu_fallback().map_err(|e| e.to_string())?;
    }
    match accelerator.dispatch() {
        None => {}
        Some(Dispatch::BuiltIn(dispatch)) => {
            builder = builder.with_execution_providers([dispatch]).map_err(|e| e.to_string())?;
        }
        Some(Dispatch::Device(ep)) => {
            // A plugin provider is appended through the devices the runtime
            // discovered for it. Every device of that provider's name, which is
            // every adapter the plugin found: one on nearly every machine.
            let env = ort::environment::Environment::current().map_err(|e| e.to_string())?;
            builder = builder
                .with_devices(env.devices().filter(|device| device.ep().ok() == Some(ep)), None)
                .map_err(|e| e.to_string())?;
        }
    }
    builder.commit_from_file(model).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::package::Arch;

    const ALL: [Accelerator; 4] =
        [Accelerator::CoreMl, Accelerator::DirectMl, Accelerator::Cuda, Accelerator::WebGpu];

    #[test]
    #[ignore = "manual real graph and runtime placement check"]
    fn forced_graph_backend_never_uses_cpu() {
        let runtime = std::env::var("RT_RUNTIME").expect("RT_RUNTIME");
        let graph = std::env::var("RT_GRAPH").expect("RT_GRAPH");
        let profile = match std::env::var("RT_PROFILE").as_deref() {
            Ok("ctd") => &DETECTOR,
            Ok("rt-small") => &BALLOON,
            Ok("rt-full") => &FULL_RT,
            // Builds with the CPU fallback on WebGPU, which is the placement
            // its timing was taken with - `partitioned_on`.
            Ok("lama") => &LAMA,
            _ => panic!("RT_PROFILE must be ctd, rt-small, rt-full or lama"),
        };
        let backend = match std::env::var("RT_BACKEND").as_deref() {
            Ok("coreml") => Accelerator::CoreMl,
            Ok("webgpu") => Accelerator::WebGpu,
            _ => panic!("RT_BACKEND must be coreml or webgpu"),
        };
        crate::runtime::load(Path::new(&runtime)).unwrap();
        match open_session(Path::new(&graph), profile, Preference::Force(backend), None) {
            Ok((_, selected)) => {
                assert_eq!(selected.accelerator, backend);
                let how = if profile.partitioned_on.contains(&backend) { "with CPU fallback" } else { "fully" };
                eprintln!("{} assigned {how} to {backend:?}", profile.name);
            }
            Err(error) => eprintln!("{} rejected {backend:?} without CPU fallback: {error}", profile.name),
        }
    }

    /// Forcing a provider builds strict, except where the profile says the
    /// graph cannot be placed on it whole. LaMa leaves its shape ops on the
    /// CPU on every GPU provider, and strict placement refuses that graph.
    #[test]
    fn a_forced_provider_is_strict_unless_the_graph_partitions_there() {
        let webgpu = Preference::Force(Accelerator::WebGpu);
        for gpu in [Accelerator::WebGpu, Accelerator::DirectMl, Accelerator::Cuda] {
            assert!(!strict_placement(&LAMA, Preference::Force(gpu), gpu), "{gpu:?}");
        }
        assert!(strict_placement(&LAMA, Preference::Force(Accelerator::CoreMl), Accelerator::CoreMl));
        assert!(strict_placement(&LAMA, Preference::Force(Accelerator::TensorRt), Accelerator::TensorRt));
        for profile in [&DETECTOR, &BALLOON, &FULL_RT, &SCRIPT_ID, &OCR] {
            assert!(strict_placement(profile, webgpu, Accelerator::WebGpu), "{}", profile.name);
        }
        // Automatic already keeps the fallback, and `CpuOnly` has nothing to
        // fall back from.
        assert!(!strict_placement(&DETECTOR, Preference::Automatic, Accelerator::WebGpu));
        assert!(!strict_placement(&LAMA, Preference::CpuOnly, Accelerator::Cpu));
    }

    /// The device list answers three ways, not two, and the third is the one
    /// that keeps this check from costing anybody a provider: a list with no
    /// CPU device in it is a list nobody produced - `ort` returns an empty
    /// iterator when `GetEpDevices` fails - and it declines nothing.
    ///
    /// The two device names below are the ones measured on this machine, off
    /// the shipped 1.28 macOS runtime.
    #[test]
    fn an_unreadable_device_list_declines_nothing() {
        let measured = ["CPUExecutionProvider", "WebGpuExecutionProvider"];
        assert!(listed_or_unreadable(measured, Accelerator::WebGpu.ort_name()));
        // Present, readable, and DirectML is genuinely not in it. This is the
        // answer a Windows machine with no Direct3D 12 adapter should give.
        assert!(!listed_or_unreadable(measured, Accelerator::DirectMl.ort_name()));
        // Nothing at all: `GetEpDevices` was not answered, so nothing is known
        // and nothing is refused.
        assert!(listed_or_unreadable([], Accelerator::DirectMl.ort_name()));
    }

    /// Only providers that publish `OrtEpDevice`s are asked about them. CoreML
    /// is the case that proves it matters: it is in the macOS build but absent
    /// from that runtime's device list. WebGPU publishes GPU devices both as a
    /// built-in provider and as a plugin.
    #[test]
    fn the_device_check_is_asked_only_of_providers_that_publish_devices() {
        assert!(Accelerator::DirectMl.publishes_devices());
        for quiet in [
            Accelerator::Cpu,
            Accelerator::CoreMl,
            Accelerator::Cuda,
            Accelerator::TensorRt,
            Accelerator::Rocm,
            Accelerator::OpenVino,
            Accelerator::Xnnpack,
        ] {
            assert!(!quiet.publishes_devices(), "{quiet:?}");
        }
        assert!(Accelerator::WebGpu.publishes_devices());
    }

    #[test]
    fn only_three_providers_implement_the_operator_lama_needs() {
        // Read off ONNX Runtime's source tree. If this ever changes, the whole
        // of LAMA's default changes with it.
        assert!(Accelerator::Cpu.implements_dft());
        assert!(Accelerator::DirectMl.implements_dft());
        assert!(Accelerator::WebGpu.implements_dft());
        for other in [Accelerator::CoreMl, Accelerator::Cuda, Accelerator::TensorRt, Accelerator::Rocm] {
            assert!(!other.implements_dft(), "{other:?}");
        }
    }

    #[test]
    fn automatic_never_partitions_a_dft_graph() {
        // Even if CUDA were measured faster for something else, LaMa must not
        // land there by default: the partition is what made CoreML slower.
        let profile = ModelProfile {
            name: "test",
            label_key: "models.kind.inpainter",
            uses_dft: true,
            measured_better: &[Accelerator::Cuda, Accelerator::CoreMl],
            unmeasured_candidates: &[Accelerator::Cuda],
            measured_peak_rss: &[],
            partitioned_on: &[],
        };
        let chosen = choose(&profile, Preference::Automatic, &ALL);
        assert_eq!(chosen.accelerator, Accelerator::Cpu);
        assert_eq!(chosen.declined, None);

        // DirectML holds the whole graph, so it is allowed through.
        let with_dml = ModelProfile { measured_better: &[Accelerator::DirectMl], ..profile };
        assert_eq!(choose(&with_dml, Preference::Automatic, &ALL).accelerator, Accelerator::DirectMl);
    }

    #[test]
    fn automatic_takes_the_first_measured_winner_that_exists() {
        assert_eq!(choose(&DETECTOR, Preference::Automatic, &ALL).accelerator, Accelerator::WebGpu);
        // CoreML is measured fast only with hidden CPU partitions. Without
        // WebGPU, Automatic takes the first unmeasured candidate or CPU.
        let windows = [Accelerator::DirectMl, Accelerator::Cuda, Accelerator::WebGpu];
        assert_eq!(choose(&DETECTOR, Preference::Automatic, &windows).accelerator, Accelerator::WebGpu);
        // DirectML is the detector's first unmeasured candidate, so it is taken
        // - flagged - rather than the machine idling its GPU.
        let bare = [Accelerator::DirectMl, Accelerator::Cuda];
        let guessed = choose(&DETECTOR, Preference::Automatic, &bare);
        assert_eq!(guessed.accelerator, Accelerator::DirectMl);
        assert!(!guessed.is_measured());

        // With neither a measured winner nor a candidate, the CPU.
        let rocm_only = [Accelerator::Rocm];
        assert_eq!(choose(&DETECTOR, Preference::Automatic, &rocm_only).accelerator, Accelerator::Cpu);
    }

    /// **The two Windows machines, end to end.** Neither was executed; both are
    /// what `Automatic` does with the provider list each flavour produces, and
    /// the difference between them is the whole of why DirectML is the default
    /// download ([`crate::runtime::package`]).
    #[test]
    fn the_two_windows_flavours_route_every_model_the_way_the_package_table_promises() {
        // The DirectML download: one provider, all three GPU vendors, and it
        // has the `DFT` kernel - so every model that belongs on a GPU is on one,
        // the inpainter included. This is the row that makes Windows as busy as
        // a Mac.
        let directml = [Accelerator::DirectMl];
        for profile in [LAMA, DETECTOR] {
            let chosen = choose(&profile, Preference::Automatic, &directml);
            assert_eq!(chosen.accelerator, Accelerator::DirectMl, "{}", profile.name);
            assert_eq!(chosen.note, Some("accel.chosen.unmeasured"), "{}", profile.name);
            assert_eq!(chosen.declined, None, "{}", profile.name);
        }

        // The CUDA download: TensorRT rides along in the same archive, and
        // the archive alone has no operator for the inpainter, so on its own
        // it sends LaMa **back to the CPU**. Nothing here may quietly put LaMa
        // on CUDA.
        let cuda = [Accelerator::Cuda, Accelerator::TensorRt];
        assert_eq!(choose(&LAMA, Preference::Automatic, &cuda).accelerator, Accelerator::Cpu);
        let chosen = choose(&DETECTOR, Preference::Automatic, &cuda);
        assert_eq!(chosen.accelerator, Accelerator::Cuda, "{}", DETECTOR.name);
        assert!(!chosen.is_measured(), "{}", DETECTOR.name);

        // With the WebGPU plugin the package unpacks beside it, the inpainter
        // has its provider back - and the detector, on this platform, stays
        // where the flavour put it. Both guesses, both said so.
        let windows = Some(Platform { os: Os::Windows, arch: Arch::X86_64 });
        let cuda_with_plugin = [Accelerator::Cuda, Accelerator::TensorRt, Accelerator::WebGpu];
        let lama = choose_on(&LAMA, Preference::Automatic, &cuda_with_plugin, None, windows);
        assert_eq!(lama.accelerator, Accelerator::WebGpu);
        assert_eq!(lama.note, Some("accel.chosen.unmeasured"));
        let detector = choose_on(&DETECTOR, Preference::Automatic, &cuda_with_plugin, None, windows);
        assert_eq!(detector.accelerator, Accelerator::Cuda);
        assert_eq!(detector.note, Some("accel.chosen.unmeasured"));

        // And the small models stay on the CPU on all of them, which is the
        // one thing that *was* measured about them.
        for available in [directml.as_slice(), cuda.as_slice(), cuda_with_plugin.as_slice()] {
            for profile in [BALLOON, SCRIPT_ID] {
                assert_eq!(
                    choose_on(&profile, Preference::Automatic, available, None, windows).accelerator,
                    Accelerator::Cpu,
                    "{}",
                    profile.name
                );
            }
        }
    }

    /// **The three Linux x64 flavours and the one aarch64 build, end to end.**
    /// Every x64 flavour keeps the inpainter on the GPU through the plugin, a
    /// CUDA flavour puts the detector on CUDA, and every one of those is a
    /// guess said aloud. Linux aarch64 is the CPU, and that is not a guess.
    #[test]
    fn every_linux_flavour_keeps_the_inpainter_on_the_gpu_except_the_one_with_no_gpu() {
        let linux = Some(Platform { os: Os::Linux, arch: Arch::X86_64 });
        let stock = [Accelerator::WebGpu];
        let cuda = [Accelerator::Cuda, Accelerator::TensorRt, Accelerator::WebGpu];

        for available in [stock.as_slice(), cuda.as_slice()] {
            let lama = choose_on(&LAMA, Preference::Automatic, available, None, linux);
            assert_eq!(lama.accelerator, Accelerator::WebGpu);
            assert_eq!(lama.note, Some("accel.chosen.unmeasured"));
            assert!(!lama.is_measured());
        }
        assert_eq!(choose_on(&DETECTOR, Preference::Automatic, &stock, None, linux).accelerator, Accelerator::WebGpu);
        assert_eq!(choose_on(&DETECTOR, Preference::Automatic, &cuda, None, linux).accelerator, Accelerator::Cuda);

        let arm = Some(Platform { os: Os::Linux, arch: Arch::Aarch64 });
        for profile in PROFILES {
            let chosen = choose_on(profile, Preference::Automatic, &[], None, arm);
            assert_eq!(chosen.accelerator, Accelerator::Cpu, "{}", profile.name);
            assert_eq!(chosen.note, None, "the CPU is never a guess");
        }
    }

    /// **The Mac reads the table as written, and everything else reads it as
    /// a guess.** The same list of providers, the same model, two platforms:
    /// one measured, one told it is not. And neither `CpuOnly` nor a forced
    /// provider is touched by the platform, because neither consults the
    /// table.
    #[test]
    fn off_the_measured_platform_every_automatic_gpu_choice_is_reported_as_a_guess() {
        let mac = Some(Platform { os: Os::MacOs, arch: Arch::Aarch64 });
        let windows = Some(Platform { os: Os::Windows, arch: Arch::X86_64 });

        let measured = choose_on(&LAMA, Preference::Automatic, &ALL, None, mac);
        assert_eq!(measured.accelerator, Accelerator::WebGpu);
        assert!(measured.is_measured());
        assert_eq!(measured, choose(&LAMA, Preference::Automatic, &ALL), "a Mac reads the table unchanged");

        // Windows has DirectML in the same list, and its own candidate comes
        // before the Mac's winner.
        let guessed = choose_on(&LAMA, Preference::Automatic, &ALL, None, windows);
        assert_eq!(guessed.accelerator, Accelerator::DirectMl);
        assert!(!guessed.is_measured());

        // A platform this build cannot name is unmeasured, because it is.
        assert!(!choose_on(&LAMA, Preference::Automatic, &ALL, None, None).is_measured());

        for platform in [mac, windows, None] {
            let cpu = choose_on(&LAMA, Preference::CpuOnly, &ALL, Some(0), platform);
            assert_eq!(cpu, choose_within(&LAMA, Preference::CpuOnly, &ALL, Some(0)));
            let forced = choose_on(&LAMA, Preference::Force(Accelerator::WebGpu), &ALL, None, platform);
            assert_eq!(forced, choose_within(&LAMA, Preference::Force(Accelerator::WebGpu), &ALL, None));
            assert_eq!(forced.accelerator, Accelerator::WebGpu);
        }
    }

    /// **Neither Windows provider has a memory row, and that is load-bearing.**
    /// The rule is that the gate fires on a measurement or not at all;
    /// nothing here has run on Windows; so a `Force` onto either is
    /// honoured however little room the machine reports. A guessed bound
    /// would decline a user's setting on arithmetic nobody has done.
    #[test]
    fn the_windows_providers_have_no_measured_peak_and_are_therefore_never_gated() {
        for profile in PROFILES {
            for provider in [Accelerator::DirectMl, Accelerator::Cuda, Accelerator::TensorRt] {
                assert_eq!(profile.peak_on(provider), None, "{} on {provider:?}", profile.name);
                let forced =
                    choose_within(profile, Preference::Force(provider), &ALL, Some(0));
                if ALL.contains(&provider) {
                    assert_eq!(forced.accelerator, provider, "{}", profile.name);
                }
            }
        }
    }

    /// Every model the application routes is in [`PROFILES`], and every one of
    /// them names itself with a catalogue key. The settings panel lists this
    /// array; a model missing from it is a model whose provider nobody can see.
    #[test]
    fn every_profile_is_listed_and_names_itself_with_a_key() {
        assert_eq!(PROFILES.len(), 6);
        for profile in PROFILES {
            assert!(profile.label_key.starts_with("models.kind."), "{}", profile.name);
        }
        let names: Vec<&str> = PROFILES.iter().map(|p| p.name).collect();
        assert_eq!(names, ["detector", "balloon", "script-id", "lama", "ocr", "hayai"]);
    }

    /// The three absences are separate sentences, and `KNOWN` is the list the
    /// interface iterates.
    #[test]
    fn a_missing_vendor_runtime_is_a_different_refusal_from_a_missing_provider() {
        assert_eq!(Unavailable::NotInRuntime.reason_key(), "accel.declined.unavailable");
        assert_eq!(Unavailable::MissingDependency.reason_key(), "accel.declined.missingRuntime");
        assert_eq!(Unavailable::NoDevice.reason_key(), "accel.declined.noDevice");

        // Only the CUDA family is checked against the machine. ROCm and
        // OpenVINO are not, deliberately - nothing here downloads them.
        for provider in [Accelerator::Cuda, Accelerator::TensorRt] {
            assert!(provider.needs_cuda_runtime(), "{provider:?}");
        }
        for provider in [Accelerator::Cpu, Accelerator::DirectMl, Accelerator::WebGpu, Accelerator::Rocm, Accelerator::OpenVino] {
            assert!(!provider.needs_cuda_runtime(), "{provider:?}");
        }

        assert_eq!(KNOWN.len(), 9);
        assert!(KNOWN.contains(&Accelerator::Cpu));
    }

    /// The filename rule, which is the whole of the CUDA-install probe.
    #[test]
    fn the_cuda_probe_recognises_the_runtime_library_and_nothing_else() {
        for name in ["cudart64_12.dll", "cudart64_13.dll", "CUDART64_12.DLL", "libcudart.so", "libcudart.so.13"] {
            assert!(is_cuda_runtime_library(name), "{name}");
        }
        for name in ["cudnn64_9.dll", "onnxruntime_providers_cuda.dll", "libcuda.so.1", "cudart64_12.lib"] {
            assert!(!is_cuda_runtime_library(name), "{name}");
        }
        for name in ["cudnn64_9.dll", "libcudnn.so.9", "libcudnn.so.9.10.2"] {
            assert!(is_cudnn_runtime_library(name), "{name}");
        }
        for name in ["cudnn64_8.dll", "libcudnn.so.8", "libcudnn.so", "libcudart.so.13"] {
            assert!(!is_cudnn_runtime_library(name), "{name}");
        }

        // A directory list with nothing in it, and one that does not exist, are
        // both "no CUDA here" rather than a panic.
        assert!(!cuda_runtime_in(&[]));
        assert!(!cuda_runtime_in(&[PathBuf::from("/definitely/not/here")]));
        assert!(!cuda_runtime_in(&[PathBuf::from(env!("CARGO_MANIFEST_DIR"))]));
        let dir = std::env::temp_dir().join(format!("manga-cleaner-cuda-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("libcudart.so.13"), []).unwrap();
        assert!(!cuda_runtime_in(std::slice::from_ref(&dir)), "cudart alone is insufficient");
        std::fs::write(dir.join("libcudnn.so.9"), []).unwrap();
        assert!(cuda_runtime_in(std::slice::from_ref(&dir)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_inpainter_goes_to_the_gpu_that_has_the_operator() {
        // Measured 1311 ms against 2540 ms. The rule that keeps it off CoreML
        // is the same rule that lets it onto WebGPU.
        assert_eq!(choose(&LAMA, Preference::Automatic, &ALL).accelerator, Accelerator::WebGpu);
        let apple_only = [Accelerator::CoreMl];
        assert_eq!(choose(&LAMA, Preference::Automatic, &apple_only).accelerator, Accelerator::Cpu);
    }

    #[test]
    fn an_unmeasured_candidate_is_used_only_as_a_last_resort_and_is_flagged() {
        // Windows: DirectML is present, WebGPU is not, and nobody has timed
        // LaMa on it. Better than idling the GPU the download was made for -
        // and the interface is told the number is not a measurement.
        let windows = [Accelerator::DirectMl];
        let chosen = choose(&LAMA, Preference::Automatic, &windows);
        assert_eq!(chosen.accelerator, Accelerator::DirectMl);
        assert_eq!(chosen.note, Some("accel.chosen.unmeasured"));
        assert!(!chosen.is_measured());

        // With a measured winner present, the guess is not reached.
        let mac = [Accelerator::WebGpu, Accelerator::DirectMl];
        let measured = choose(&LAMA, Preference::Automatic, &mac);
        assert_eq!(measured.accelerator, Accelerator::WebGpu);
        assert!(measured.is_measured());
    }

    #[test]
    fn nothing_measured_and_nothing_plausible_means_the_cpu() {
        for profile in [BALLOON, SCRIPT_ID] {
            assert_eq!(
                choose(&profile, Preference::Automatic, &ALL).accelerator,
                Accelerator::Cpu,
                "{}",
                profile.name
            );
        }
    }

    #[test]
    fn a_forced_provider_is_honoured_and_a_missing_one_is_reported() {
        let forced = choose(&DETECTOR, Preference::Force(Accelerator::Cuda), &ALL);
        assert_eq!(forced.accelerator, Accelerator::Cuda);
        assert_eq!(forced.declined, None);

        let missing = choose(&DETECTOR, Preference::Force(Accelerator::Rocm), &ALL);
        assert_eq!(missing.accelerator, Accelerator::Cpu);
        assert_eq!(missing.declined.unwrap().reason_key, "accel.declined.unavailable");
    }

    #[test]
    fn forcing_a_partitioning_provider_onto_lama_works_and_says_so() {
        // The user asked for it, it will run, and it will be slower. Both
        // halves of that are the point.
        let forced = choose(&LAMA, Preference::Force(Accelerator::CoreMl), &ALL);
        assert_eq!(forced.accelerator, Accelerator::CoreMl);
        assert_eq!(forced.declined.unwrap().reason_key, "accel.declined.partitioned");
    }

    #[test]
    fn cpu_only_is_absolute() {
        for profile in [DETECTOR, LAMA] {
            let chosen = choose(&profile, Preference::CpuOnly, &ALL);
            assert_eq!(chosen.accelerator, Accelerator::Cpu);
            assert_eq!(chosen.declined, None);
        }
    }

    /// **The guard's edge case.** `CpuOnly` is absolute and it is absolute
    /// *before* the memory gate - a machine with no room at all still runs
    /// on the CPU rather than being told there is nowhere to go, because
    /// the CPU is where it was going anyway.
    #[test]
    fn cpu_only_is_absolute_under_any_amount_of_pressure() {
        for profile in [DETECTOR, LAMA] {
            let chosen = choose_within(&profile, Preference::CpuOnly, &ALL, Some(0));
            assert_eq!(chosen.accelerator, Accelerator::Cpu, "{}", profile.name);
            assert_eq!(chosen.declined, None, "{}", profile.name);
        }
    }

    /// The forced-provider case, on a machine that cannot carry it.
    #[test]
    fn a_forced_provider_that_would_not_fit_is_declined_with_its_figure() {
        // An 8 GB Mac: two thirds of it is the working set, less the webview.
        let room = 8 * memory::GIB / 3 * 2 - memory::WEBVIEW_BYTES;

        let lama = choose_within(&LAMA, Preference::Force(Accelerator::CoreMl), &ALL, Some(room));
        assert_eq!(lama.accelerator, Accelerator::Cpu, "the 8.19 GB row was honoured on an 8 GB machine");
        let declined = lama.declined.expect("declined");
        assert_eq!(declined.reason_key, "accel.declined.memory");
        assert_eq!(declined.wanted, Accelerator::CoreMl);
        // The figure is on the outcome, so the interface can say how much.
        assert_eq!(declined.needed_bytes, Some(8387 * memory::MIB));
        assert_eq!(declined.room_bytes, Some(room));

        // And the rows that are not the problem are not touched by the guard.
        let webgpu = choose_within(&LAMA, Preference::Force(Accelerator::WebGpu), &ALL, Some(room));
        assert_eq!(webgpu.accelerator, Accelerator::WebGpu);
        assert_eq!(webgpu.declined, None);
    }

    /// The machine the figures were measured on carries them, and the ruling
    /// that a forced provider is honoured still holds there. A guard that
    /// declined here would be overturning the standing ruling rather than
    /// applying rule 9.
    #[test]
    fn a_forced_provider_that_fits_is_still_honoured() {
        let room = 32 * memory::GIB / 3 * 2 - memory::WEBVIEW_BYTES;
        let lama = choose_within(&LAMA, Preference::Force(Accelerator::CoreMl), &ALL, Some(room));
        assert_eq!(lama.accelerator, Accelerator::CoreMl);
        // Still reported, and under the *speed* reason: the partition is why it
        // is a bad idea on this machine, and it is a different sentence.
        assert_eq!(lama.declined.unwrap().reason_key, "accel.declined.partitioned");
    }

    /// **An unmeasured provider, and an unmeasured machine, are honoured.**
    /// Both halves of that: a gate that guessed would decline a user's setting
    /// on arithmetic nobody has done, and Windows and Linux have never
    /// been executed here.
    #[test]
    fn nothing_is_declined_on_a_number_nobody_has() {
        // No room figure - a probe that failed, or a caller that did not ask.
        let unknown_machine =
            choose_within(&LAMA, Preference::Force(Accelerator::CoreMl), &ALL, None);
        assert_eq!(unknown_machine.accelerator, Accelerator::CoreMl);

        // A machine with no room, and a provider with no measurement for this
        // model. DirectML is unmeasured for LaMa on purpose - it is the
        // Windows path - and it is honoured rather than guessed at.
        assert_eq!(LAMA.peak_on(Accelerator::DirectMl), None);
        let unmeasured =
            choose_within(&LAMA, Preference::Force(Accelerator::DirectMl), &ALL, Some(0));
        assert_eq!(unmeasured.accelerator, Accelerator::DirectMl);
        assert_eq!(unmeasured.declined, None);
    }

    /// `Automatic` never reached either figure and still does not. The guard is
    /// a bound on what a *forced* provider may cost, not a second router.
    #[test]
    fn the_memory_gate_does_not_change_an_automatic_choice() {
        for profile in [LAMA, DETECTOR] {
            let squeezed = choose_within(&profile, Preference::Automatic, &ALL, Some(0));
            let free = choose(&profile, Preference::Automatic, &ALL);
            assert_eq!(squeezed.accelerator, free.accelerator, "{}", profile.name);
        }
        // Strict placement found CPU nodes in the CoreML detector graph, so
        // Automatic uses WebGPU even under a low memory figure.
        assert_eq!(choose_within(&DETECTOR, Preference::Automatic, &ALL, Some(0)).accelerator, Accelerator::WebGpu);
        assert_ne!(
            choose_within(&LAMA, Preference::Automatic, &ALL, Some(0)).accelerator,
            Accelerator::CoreMl,
            "{}",
            LAMA.name
        );
    }

    #[test]
    fn the_runtime_on_this_machine_reports_what_it_has() {
        // Not an assertion about which providers exist - that is the machine's
        // business - only that asking does not panic and that the CPU is always
        // in the list.
        let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../runtimes/onnxruntime-osx-arm64-1.28.0/lib")
            .join(crate::runtime::DYLIB_NAME);
        if !dev.exists() {
            eprintln!("skipped: no runtime - run scripts/fetch-runtime.sh");
            return;
        }
        crate::runtime::load(&dev).expect("the runtime loads");
        let found = available();
        assert!(found.contains(&Accelerator::Cpu));
        eprintln!("available: {found:?}");
    }
}
