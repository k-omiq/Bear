//! Which ONNX Runtime a machine needs, and what is inside it.
//!
//! The runtime is fetched after install rather than bundled, so "which
//! build" is a decision the application makes on first launch. It is not
//! one build: Microsoft publishes
//! a different archive per platform, and **they do not carry the same execution
//! providers**.
//!
//! That is the reason this module exists rather than a URL template, and it is
//! a correction. The claim that "WebGPU ships in the stock ONNX Runtime build
//! on every platform" was true of the build measured - macOS - and false
//! everywhere else. Verified by `scripts/verify-runtime-packages.sh`, which
//! reads each archive's **`include/*_provider_factory.h` and its provider
//! libraries** - a header ships only where the provider was built and a
//! `onnxruntime_providers_*` library only where one was, while the provider
//! *name strings* are in every build and prove nothing:
//!
//! | package | size | providers |
//! |---|---|---|
//! | `onnxruntime-osx-arm64-1.28.0` | 32 MB | cpu, **coreml**, **webgpu** |
//! | `onnxruntime-win-x64-1.28.0` | 79 MB | cpu |
//! | `onnxruntime-win-arm64-1.28.0` | 80 MB | cpu |
//! | `onnxruntime-linux-x64-1.28.0` | 9 MB | cpu |
//! | `onnxruntime-linux-aarch64-1.28.0` | 8 MB | cpu |
//! | `Microsoft.ML.OnnxRuntime.DirectML` 1.24.4 | 13 MB | cpu, **dml** |
//! | `Microsoft.AI.DirectML` 1.15.4 | 202 MB | *(`DirectML.dll` only)* |
//! | `onnxruntime-win-x64-gpu_cuda12-1.28.0` | 455 MB | cpu, **cuda**, **tensorrt** |
//! | `onnxruntime-win-x64-gpu_cuda13-1.28.0` | 366 MB | cpu, **cuda**, **tensorrt** |
//! | `onnxruntime-linux-x64-gpu_cuda12-1.28.0` | 424 MB | cpu, **cuda**, **tensorrt** |
//! | `onnxruntime-linux-x64-gpu_cuda13-1.28.0` | 241 MB | cpu, **cuda**, **tensorrt** |
//! | `Microsoft.ML.OnnxRuntime.EP.WebGpu` 0.3.0 | 39 MB | **webgpu** *(plugin: win-x64, win-arm64, linux-x64, osx-arm64)* |
//!
//! Those sizes are on the table itself now, as [`Artefact::bytes`], read from
//! each host's `Content-Length` rather than rounded off a directory listing -
//! so the runtime row can say what its download costs before anything is
//! downloaded, and the flavour picker can put the two numbers side by side.
//!
//! So the GPU path differs by platform and neither choice is ours: **macOS gets
//! WebGPU built in, Windows gets DirectML, and the stock Windows and Linux
//! archives get nothing.** CoreML splits the inpainter's graph into CPU
//! islands, so on a Mac WebGPU is what takes it off the CPU
//! ([`crate::accel`]).
//!
//! ## The WebGPU plugin closes the gap on Windows and Linux
//!
//! From 1.28 the WebGPU provider is also published as a **plugin library**,
//! `Microsoft.ML.OnnxRuntime.EP.WebGpu`, registered into any runtime at
//! 1.24.4 or later ([`crate::runtime::load`] does it), and built for win-x64,
//! win-arm64, linux-x64 and osx-arm64. It is Dawn over Direct3D 12 on Windows
//! and over Vulkan on Linux, so **it reaches NVIDIA, AMD and Intel alike** and
//! is the one GPU provider for the inpainter that every platform but Linux
//! aarch64 can have. Every Windows and Linux x64 row below carries it as a
//! companion artefact: on the DirectML and CUDA flavours it is the fallback
//! when the flavour's own provider is not usable, and on the stock Linux build
//! it is the whole of the GPU path. On
//! Linux it needs the system Vulkan loader, `libvulkan.so.1`, which every GPU
//! driver package installs and which is not listed under
//! [`Package::user_installed`] for that reason - a machine without it reads
//! `accel.declined.noDevice` rather than being refused the download. macOS
//! does not carry the plugin: its archive has the provider built in, and that
//! is the one that was measured.
//!
//! ## A platform has flavours, and exactly one of them is the default
//!
//! Windows is the platform where "which archive" stopped being one answer.
//! **DirectML is the default and reaches every vendor** - Direct3D 12, so
//! NVIDIA, AMD and Intel alike, nothing for the user to install, and it is the
//! inpainter's first candidate, so the inpainter runs on the GPU there the way
//! it does on a Mac. That is the whole of what a Windows machine needs to be as busy as an
//! Apple one.
//!
//! It is not the small download the earlier note claimed, and the arithmetic is
//! worth writing down because it is the one against CUDA: the ONNX Runtime half
//! is 13 MB and `Microsoft.AI.DirectML` is **202 MB**, of which one 18 MB
//! `DirectML.dll` is used - the package carries eight architectures, three of
//! them Xbox. So Windows costs about 215 MB either way and CUDA costs 366–455
//! MB *plus* a toolkit the user installs. Size is therefore the weaker half of
//! the argument; reaching every vendor with nothing to install is the whole of
//! it.
//!
//! CUDA is a **second flavour, offered and never chosen for anyone**, which
//! corrects an earlier "declined" without reversing its reasoning. Three
//! facts decide it, all verified by listing the
//! archives rather than inferred:
//!
//! - The CUDA archive's `onnxruntime.dll` is a **different build** from the
//!   DirectML one and carries no `DmlExecutionProvider`. [`crate::runtime::load`]
//!   loads exactly one library, so taking CUDA means giving DirectML up.
//! - It reaches NVIDIA only. `Automatic` puts the detector and LaMa on it as
//!   unmeasured candidates ([`crate::accel`]); the WebGPU plugin beside it is
//!   the fallback when CUDA is not usable.
//! - It carries `onnxruntime_providers_cuda.dll` and nothing else: **no
//!   `cudart`, no cuDNN**. The user installs those, and
//!   [`crate::accel::Accelerator::availability`] says which is missing.
//!
//! Two CUDA rows rather than one, because ONNX Runtime publishes two and a
//! machine has whichever toolkit it has. Neither has been run here.
//!
//! **Linux x64 has the same three flavours**, for the same reasons: the stock
//! build with the WebGPU plugin is the default and reaches every vendor with
//! nothing to install, and the two CUDA archives are offered to an NVIDIA
//! machine whose owner has the toolkit. Linux aarch64 has the stock build and
//! nothing else - no CUDA archive and no plugin are published for it.
//!
//! Two things still deliberately not taken:
//!
//! - **ROCm, MIGraphX and OpenVINO.** Microsoft publishes no archive carrying
//!   any of them; AMD and Intel ship theirs as Python wheels only. An AMD or
//!   Intel GPU on Linux reaches this application through WebGPU, which is a
//!   real path and the only one there is to download.
//! - **Intel macOS.** 1.28.0 publishes no `osx-x86_64` archive at all. §9 makes
//!   macOS **arm64** the first-release target, so this is consistent rather than
//!   a gap; an Intel Mac would need an older release pinned for it.
//!
//! Nothing here decides what the application *uses*. The running process asks
//! the loaded runtime through [`crate::accel::available`]; this table only
//! decides what to download.

/// A machine, as far as the download is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Windows,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Platform {
    /// This machine, at compile time. The download happens on the machine it is
    /// for; the rest of the table exists for the tests and for anything that
    /// mirrors the archives.
    pub const fn host() -> Option<Platform> {
        let os = if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            return None;
        };
        let arch = if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Arch::Aarch64
        } else {
            return None;
        };
        Some(Platform { os, arch })
    }
}

/// One downloadable artefact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Artefact {
    pub url: &'static str,
    /// sha256 of the artefact as downloaded. Never empty: an artefact without a
    /// verified digest does not belong in this table.
    pub sha256: &'static str,
    /// The published archive's exact size, from the host's `Content-Length`.
    ///
    /// Not for the progress bar - the response's own `Content-Length` is better
    /// for that, because it is the answer for the transfer actually happening.
    /// It is here so the **runtime row can say how large its download is before
    /// anything is downloaded**, the way a weight's row does, and so a
    /// flavour picker can put 455 MB against 215 MB in front of the person
    /// choosing.
    pub bytes: u64,
    /// The path inside the archive holding the loadable library.
    pub library_dir: &'static str,
}

/// Which build of ONNX Runtime, where a platform publishes more than one.
///
/// Not a preference and not a provider: it is the *archive*, and the providers
/// inside it are [`Package::advertised`]. Exactly one flavour per platform is
/// [`Package::default_flavour`]; the rest are offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// Whatever Microsoft compiled into the general release for this platform.
    /// macOS carries CoreML and WebGPU; Linux carries the CPU only and gets
    /// the WebGPU plugin beside it on x64.
    Stock,
    /// The Direct3D 12 build: every GPU on Windows through one provider, with
    /// nothing for the user to install.
    DirectMl,
    /// NVIDIA, built against CUDA 12.x. Needs a CUDA install this application
    /// does not ship, of 12.8 or later (`cuda::minor_floor`).
    Cuda12,
    /// The same, built against CUDA 13.x.
    Cuda13,
}

impl Flavour {
    /// The identifier the setting and the downloader use. Data, not copy - a
    /// build's name is not translated, the same way a provider's is not.
    pub fn id(self) -> &'static str {
        match self {
            Flavour::Stock => "stock",
            Flavour::DirectMl => "directml",
            Flavour::Cuda12 => "cuda12",
            Flavour::Cuda13 => "cuda13",
        }
    }

    pub fn from_id(id: &str) -> Option<Flavour> {
        [Flavour::Stock, Flavour::DirectMl, Flavour::Cuda12, Flavour::Cuda13]
            .into_iter()
            .find(|flavour| flavour.id() == id)
    }
}

/// What one platform needs, in one flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Package {
    pub platform: Platform,
    pub flavour: Flavour,
    /// Whether this is the one a machine gets without being asked. Exactly one
    /// row per platform sets it, which
    /// `each_platform_has_exactly_one_default` pins.
    pub default_flavour: bool,
    /// The ONNX Runtime version this artefact contains. Not the same across
    /// platforms - the DirectML build trails the stock releases - and `ort`
    /// accepts anything at or above its `api-NN` floor, which is 1.22.
    pub ort_version: &'static str,
    pub runtime: Artefact,
    /// Further artefacts unpacked into the same directory. Two kinds: a
    /// library the runtime cannot load without - Windows needs `DirectML.dll`,
    /// which `Microsoft.ML.OnnxRuntime.DirectML` declares a dependency on and
    /// does not include - and the WebGPU plugin, which the runtime can load
    /// without and this application cannot put the inpainter on a GPU
    /// without.
    pub companions: &'static [Artefact],
    /// Providers this archive was **observed** to carry, from its provider
    /// factory headers and provider libraries. Advisory: the running process
    /// still asks the runtime.
    pub advertised: &'static [&'static str],
    /// What the **user** must install before this package's GPU provider works,
    /// as version strings rather than catalogue keys - a toolkit's name and
    /// version is data, like a licence identifier or a model name, and the
    /// interface interpolates it rather than looking it up.
    ///
    /// Empty for every flavour but CUDA. That emptiness is the argument for
    /// DirectML being the default: it is the GPU path with nothing behind it.
    pub user_installed: &'static [&'static str],
}

impl Package {
    /// Everything to download, runtime first.
    pub fn artefacts(&self) -> Vec<&Artefact> {
        let mut all = vec![&self.runtime];
        all.extend(self.companions.iter());
        all
    }

    /// Whether this archive carries a provider that is not the CPU.
    pub fn has_gpu_provider(&self) -> bool {
        self.advertised.iter().any(|name| *name != "CPUExecutionProvider")
    }

    /// How large this flavour's download is, in bytes: every artefact, because
    /// a Windows package is not usable with one of its two.
    ///
    /// This is the number the runtime row and the flavour picker show. It is
    /// what is **transferred**, not what lands on disk - only `library_dir` is
    /// unpacked and the archives are deleted afterwards, so `Microsoft.AI.DirectML`
    /// costs 202 MB of network for one 18 MB library and the WebGPU plugin
    /// 39 MB for the one platform's third of it. Reporting the download is
    /// the honest half: it is the wait and the data allowance the user is being
    /// asked for.
    pub fn bytes(&self) -> u64 {
        self.artefacts().iter().map(|artefact| artefact.bytes).sum()
    }
}

/// `Microsoft.AI.DirectML` for one architecture: one nupkg, eight
/// architectures inside it, and `library_dir` is which one is unpacked.
const fn directml_dll(library_dir: &'static str) -> Artefact {
    Artefact {
        url: concat!(
            "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml",
            "/1.15.4/microsoft.ai.directml.1.15.4.nupkg"
        ),
        sha256: "4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9",
        bytes: 202_292_617,
        library_dir,
    }
}

/// The WebGPU plugin for one platform. One nupkg for all four, so the digest
/// and the size are the same on every row and only `library_dir` differs.
/// Windows rows also get `dxcompiler.dll` and `dxil.dll` out of the same
/// directory, which the plugin's Direct3D 12 backend loads by name.
const fn webgpu_plugin(library_dir: &'static str) -> Artefact {
    Artefact {
        url: concat!(
            "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.ep.webgpu",
            "/0.3.0/microsoft.ml.onnxruntime.ep.webgpu.0.3.0.nupkg"
        ),
        sha256: "8c96476bf982b405bc9192fcf886e7b5b7e7398090df483edf07090bf58583c3",
        bytes: 39_485_572,
        library_dir,
    }
}

/// The plugin's version. Separate from [`Package::ort_version`] because it is
/// a different package on a different cadence - 0.3.0 registers into any
/// runtime from 1.24.4 up, which is every row that carries it.
pub const WEBGPU_PLUGIN_VERSION: &str = "0.3.0";

const WIN_X64_COMPANIONS: &[Artefact] =
    &[directml_dll("bin/x64-win"), webgpu_plugin("runtimes/win-x64/native")];
const WIN_ARM64_COMPANIONS: &[Artefact] =
    &[directml_dll("bin/arm64-win"), webgpu_plugin("runtimes/win-arm64/native")];
const WIN_X64_PLUGIN_ONLY: &[Artefact] = &[webgpu_plugin("runtimes/win-x64/native")];
const LINUX_X64_PLUGIN_ONLY: &[Artefact] = &[webgpu_plugin("runtimes/linux-x64/native")];

pub const PACKAGES: &[Package] = &[
    Package {
        platform: Platform { os: Os::MacOs, arch: Arch::Aarch64 },
        flavour: Flavour::Stock,
        default_flavour: true,
        ort_version: "1.28.0",
        runtime: Artefact {
            url: concat!(
                "https://github.com/microsoft/onnxruntime/releases/download",
                "/v1.28.0/onnxruntime-osx-arm64-1.28.0.tgz"
            ),
            sha256: "1268b359718099bde2cedb55787f182a130067bc4f31e8c88478c445b850d3d8",
            bytes: 32_396_562,
            library_dir: "onnxruntime-osx-arm64-1.28.0/lib",
        },
        companions: &[],
        advertised: &["CPUExecutionProvider", "CoreMLExecutionProvider", "WebGpuExecutionProvider"],
        user_installed: &[],
    },
    Package {
        platform: Platform { os: Os::Windows, arch: Arch::X86_64 },
        flavour: Flavour::DirectMl,
        default_flavour: true,
        ort_version: "1.24.4",
        runtime: Artefact {
            url: concat!(
                "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml",
                "/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg"
            ),
            sha256: "57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681",
            bytes: 12_458_649,
            library_dir: "runtimes/win-x64/native",
        },
        companions: WIN_X64_COMPANIONS,
        advertised: &["CPUExecutionProvider", "DmlExecutionProvider", "WebGpuExecutionProvider"],
        user_installed: &[],
    },
    // The two CUDA flavours. Same release as the stock macOS pin, one per CUDA
    // major, because a machine has whichever toolkit it has and ONNX Runtime
    // publishes both. Neither is a default anywhere: see the module docs for
    // why an NVIDIA machine is still better off on DirectML unless its owner has
    // measured otherwise.
    Package {
        platform: Platform { os: Os::Windows, arch: Arch::X86_64 },
        flavour: Flavour::Cuda12,
        default_flavour: false,
        ort_version: "1.28.0",
        runtime: Artefact {
            url: concat!(
                "https://github.com/microsoft/onnxruntime/releases/download",
                "/v1.28.0/onnxruntime-win-x64-gpu_cuda12-1.28.0.zip"
            ),
            sha256: "6b7bf16d6d30180db7f386fb179aa4e4f1313f0924531a2879b7b090b56518c1",
            bytes: 455_344_532,
            library_dir: "onnxruntime-win-x64-gpu_cuda12-1.28.0/lib",
        },
        companions: WIN_X64_PLUGIN_ONLY,
        advertised: &[
            "CPUExecutionProvider",
            "CUDAExecutionProvider",
            "TensorrtExecutionProvider",
            "WebGpuExecutionProvider",
        ],
        user_installed: &["CUDA 12.8+", "cuDNN 9.x"],
    },
    Package {
        platform: Platform { os: Os::Windows, arch: Arch::X86_64 },
        flavour: Flavour::Cuda13,
        default_flavour: false,
        ort_version: "1.28.0",
        runtime: Artefact {
            url: concat!(
                "https://github.com/microsoft/onnxruntime/releases/download",
                "/v1.28.0/onnxruntime-win-x64-gpu_cuda13-1.28.0.zip"
            ),
            sha256: "137f0822a4923b1d84d3e09496e0792ebbb221eb3a61a0657f71a12ab68ab1e2",
            bytes: 365_825_268,
            library_dir: "onnxruntime-win-x64-gpu_cuda13-1.28.0/lib",
        },
        companions: WIN_X64_PLUGIN_ONLY,
        advertised: &[
            "CPUExecutionProvider",
            "CUDAExecutionProvider",
            "TensorrtExecutionProvider",
            "WebGpuExecutionProvider",
        ],
        // 1.28's own documented requirement. TensorRT wants its own libraries
        // beyond these, which is why the archive advertises the provider and
        // `accel` still expects a session build on it to fail on a machine that
        // has only CUDA.
        user_installed: &["CUDA 13.x", "cuDNN 9.x"],
    },
    Package {
        platform: Platform { os: Os::Windows, arch: Arch::Aarch64 },
        flavour: Flavour::DirectMl,
        default_flavour: true,
        ort_version: "1.24.4",
        runtime: Artefact {
            url: concat!(
                "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml",
                "/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg"
            ),
            sha256: "57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681",
            bytes: 12_458_649,
            library_dir: "runtimes/win-arm64/native",
        },
        companions: WIN_ARM64_COMPANIONS,
        advertised: &["CPUExecutionProvider", "DmlExecutionProvider", "WebGpuExecutionProvider"],
        user_installed: &[],
    },
    Package {
        platform: Platform { os: Os::Linux, arch: Arch::X86_64 },
        flavour: Flavour::Stock,
        default_flavour: true,
        ort_version: "1.28.0",
        runtime: Artefact {
            url: concat!(
                "https://github.com/microsoft/onnxruntime/releases/download",
                "/v1.28.0/onnxruntime-linux-x64-1.28.0.tgz"
            ),
            sha256: "a3e1b79d7bb1bf09696ce675f49e4064e6c81f6202b8225624fff0e93f8d6407",
            bytes: 9_125_960,
            library_dir: "onnxruntime-linux-x64-1.28.0/lib",
        },
        companions: LINUX_X64_PLUGIN_ONLY,
        advertised: &["CPUExecutionProvider", "WebGpuExecutionProvider"],
        user_installed: &[],
    },
    // Linux's two CUDA flavours, the same shape as Windows's and offered on
    // the same terms: never a default, the toolkit installed by the user, and
    // the WebGPU plugin beside them so the inpainter stays on the GPU. The
    // digests are the release's own, from the GitHub asset record.
    Package {
        platform: Platform { os: Os::Linux, arch: Arch::X86_64 },
        flavour: Flavour::Cuda12,
        default_flavour: false,
        ort_version: "1.28.0",
        runtime: Artefact {
            url: concat!(
                "https://github.com/microsoft/onnxruntime/releases/download",
                "/v1.28.0/onnxruntime-linux-x64-gpu_cuda12-1.28.0.tgz"
            ),
            sha256: "ea6bd2b65d7dfabbeb92c4af5dd8f12e5aed8601e544ad378d2f872275438b1a",
            bytes: 423_724_908,
            library_dir: "onnxruntime-linux-x64-gpu_cuda12-1.28.0/lib",
        },
        companions: LINUX_X64_PLUGIN_ONLY,
        advertised: &[
            "CPUExecutionProvider",
            "CUDAExecutionProvider",
            "TensorrtExecutionProvider",
            "WebGpuExecutionProvider",
        ],
        user_installed: &["CUDA 12.8+", "cuDNN 9.x"],
    },
    Package {
        platform: Platform { os: Os::Linux, arch: Arch::X86_64 },
        flavour: Flavour::Cuda13,
        default_flavour: false,
        ort_version: "1.28.0",
        runtime: Artefact {
            url: concat!(
                "https://github.com/microsoft/onnxruntime/releases/download",
                "/v1.28.0/onnxruntime-linux-x64-gpu_cuda13-1.28.0.tgz"
            ),
            sha256: "84d28f27589090b280d4312743efd3d450cd4ac7d1e1d75e7d9076d9637bf9de",
            bytes: 240_874_643,
            library_dir: "onnxruntime-linux-x64-gpu_cuda13-1.28.0/lib",
        },
        companions: LINUX_X64_PLUGIN_ONLY,
        advertised: &[
            "CPUExecutionProvider",
            "CUDAExecutionProvider",
            "TensorrtExecutionProvider",
            "WebGpuExecutionProvider",
        ],
        user_installed: &["CUDA 13.x", "cuDNN 9.x"],
    },
    Package {
        platform: Platform { os: Os::Linux, arch: Arch::Aarch64 },
        flavour: Flavour::Stock,
        default_flavour: true,
        ort_version: "1.28.0",
        runtime: Artefact {
            url: concat!(
                "https://github.com/microsoft/onnxruntime/releases/download",
                "/v1.28.0/onnxruntime-linux-aarch64-1.28.0.tgz"
            ),
            sha256: "e15ff8b5d85afe6c144d97c6fd432254bf76a219daaf17658087d6ecb3e8f0bb",
            bytes: 8_116_278,
            library_dir: "onnxruntime-linux-aarch64-1.28.0/lib",
        },
        // No plugin: `Microsoft.ML.OnnxRuntime.EP.WebGpu` publishes no
        // linux-arm64 library, and no CUDA archive is published either. This
        // is the one row that is still the CPU alone.
        companions: &[],
        advertised: &["CPUExecutionProvider"],
        user_installed: &[],
    },
];

/// The package a machine gets when nobody has asked for anything else.
pub fn for_platform(platform: Platform) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.platform == platform && p.default_flavour)
}

/// Every build published for one platform, default first. What the in-app
/// downloader offers.
pub fn flavours(platform: Platform) -> Vec<&'static Package> {
    let mut found: Vec<&'static Package> =
        PACKAGES.iter().filter(|p| p.platform == platform).collect();
    found.sort_by_key(|p| !p.default_flavour);
    found
}

/// One named build for one platform.
pub fn for_flavour(platform: Platform, flavour: Flavour) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.platform == platform && p.flavour == flavour)
}

/// The package one platform gets for a **stored flavour id**, falling back to
/// its default for anything that platform does not publish.
///
/// The fallback is the whole point and it is not laxness. The id comes from the
/// settings file, so it can name a flavour that was removed when a version
/// moved, one that belongs to another platform (a settings file copied from a
/// Windows machine to a Mac), or nothing at all - and every one of those has the
/// same right answer: give the machine the build it would have got if nobody had
/// ever chosen. Refusing would leave a user with a runtime they cannot download
/// and a setting they cannot see to correct.
pub fn for_platform_flavour(platform: Platform, id: &str) -> Option<&'static Package> {
    Flavour::from_id(id)
        .and_then(|flavour| for_flavour(platform, flavour))
        .or_else(|| for_platform(platform))
}

/// The package this machine needs, or `None` on a platform with no published
/// build - Intel macOS, today.
pub fn for_host() -> Option<&'static Package> {
    Platform::host().and_then(for_platform)
}

/// [`for_platform_flavour`] for this machine: what `download_runtime` fetches
/// once the `runtimeFlavour` setting has been read.
pub fn for_host_flavour(id: &str) -> Option<&'static Package> {
    Platform::host().and_then(|platform| for_platform_flavour(platform, id))
}

/// Every build this machine could be given.
pub fn host_flavours() -> Vec<&'static Package> {
    Platform::host().map(flavours).unwrap_or_default()
}

/// Whether the package a platform gets **by default** has any GPU provider at
/// all. The interface uses it to explain why the accelerator setting has one
/// entry on Linux aarch64 and three on a Mac.
///
/// Deliberately the default rather than "any flavour": Windows would answer
/// `true` either way, and the question this answers is what a machine has, not
/// what it could be talked into downloading.
pub fn has_gpu_provider(platform: Platform) -> bool {
    for_platform(platform).map(Package::has_gpu_provider).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_artefact_carries_a_real_digest() {
        for package in PACKAGES {
            for artefact in package.artefacts() {
                assert_eq!(artefact.sha256.len(), 64, "{}", artefact.url);
                assert!(
                    artefact.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                    "{}",
                    artefact.url
                );
            }
        }
    }

    /// Every artefact says how large it is, and a package's size is the sum of
    /// the artefacts a machine actually has to fetch - both of them, on the one
    /// platform that needs two. This is the number the runtime row and the
    /// flavour picker show, so it is the one that has to be arithmetic rather
    /// than a guess.
    #[test]
    fn a_packages_download_is_the_sum_of_its_artefacts() {
        for package in PACKAGES {
            for artefact in package.artefacts() {
                assert!(artefact.bytes > 0, "{}: no size", artefact.url);
            }
            let summed: u64 = package.artefacts().iter().map(|a| a.bytes).sum();
            assert_eq!(package.bytes(), summed, "{:?} {}", package.platform, package.flavour.id());
        }

        // The two figures the module docs argue with: Windows costs about 254
        // MB by default - 215 for DirectML and 39 for the WebGPU plugin - and
        // CUDA 12 costs 455 MB before its own plugin.
        let windows = Platform { os: Os::Windows, arch: Arch::X86_64 };
        let directml = for_platform(windows).expect("a default").bytes();
        assert_eq!(directml, 12_458_649 + 202_292_617 + 39_485_572);
        let cuda12 = for_flavour(windows, Flavour::Cuda12).expect("a CUDA row").bytes();
        assert!(cuda12 > directml, "the CUDA archive is the larger download");
    }

    /// A stored flavour id that this platform does not publish gets the default
    /// rather than nothing: the id comes from a settings file, which can name a
    /// flavour that was dropped, one that belongs to another platform, or
    /// nothing at all.
    #[test]
    fn an_unknown_flavour_id_falls_back_to_the_platforms_default() {
        let windows = Platform { os: Os::Windows, arch: Arch::X86_64 };
        let mac = Platform { os: Os::MacOs, arch: Arch::Aarch64 };

        // A choice this platform does publish is honoured.
        assert_eq!(
            for_platform_flavour(windows, "cuda12").map(|p| p.flavour),
            Some(Flavour::Cuda12)
        );

        // Everything else lands on the default, one case per way of being wrong.
        for id in ["", "rocm", "cuda12", "directml"] {
            assert_eq!(
                for_platform_flavour(mac, id).map(|p| p.flavour),
                Some(Flavour::Stock),
                "macOS publishes one build, whatever {id} says"
            );
        }
        assert_eq!(
            for_platform_flavour(windows, "stock").map(|p| p.flavour),
            Some(Flavour::DirectMl),
            "Windows publishes no stock build, so the default answers for it"
        );

        // And a platform with nothing published has nothing to fall back to.
        let intel_mac = Platform { os: Os::MacOs, arch: Arch::X86_64 };
        assert!(for_platform_flavour(intel_mac, "stock").is_none());
    }

    /// **What rides beside each runtime.** `DirectML.dll` on the DirectML
    /// flavour only, because `Microsoft.ML.OnnxRuntime.DirectML` declares a
    /// dependency on `Microsoft.AI.DirectML` and does not include it. The
    /// WebGPU plugin on every Windows and Linux x64 row, because that is where
    /// the provider is not built in and is published. Nothing on macOS, whose
    /// archive has it built in, and nothing on Linux aarch64, for which
    /// nothing is published. The CUDA archives themselves are self-contained  - 
    /// what *they* are missing is on the machine, not in a download.
    #[test]
    fn each_row_carries_exactly_the_companions_its_platform_needs() {
        let plugin_url = webgpu_plugin("").url;
        for package in PACKAGES {
            let has_directml = package.companions.iter().any(|a| a.library_dir.ends_with("-win"));
            let has_plugin = package.companions.iter().any(|a| a.url == plugin_url);
            let os = package.platform.os;
            let arch = package.platform.arch;
            assert_eq!(has_directml, package.flavour == Flavour::DirectMl, "{os:?} {arch:?}");
            let plugin_expected = os == Os::Windows || (os == Os::Linux && arch == Arch::X86_64);
            assert_eq!(has_plugin, plugin_expected, "{os:?} {arch:?} {}", package.flavour.id());
            assert_eq!(
                package.advertised.contains(&"WebGpuExecutionProvider"),
                has_plugin || os == Os::MacOs,
                "{os:?} {arch:?} {}",
                package.flavour.id()
            );
            assert_eq!(package.artefacts().len(), 1 + package.companions.len());
        }
        // And the plugin's directory names the platform it is unpacked for,
        // because one nupkg carries all four.
        for package in PACKAGES {
            for artefact in package.companions.iter().filter(|a| a.url == plugin_url) {
                let rid = match (package.platform.os, package.platform.arch) {
                    (Os::Windows, Arch::X86_64) => "win-x64",
                    (Os::Windows, Arch::Aarch64) => "win-arm64",
                    (Os::Linux, Arch::X86_64) => "linux-x64",
                    other => panic!("no plugin is published for {other:?}"),
                };
                assert_eq!(artefact.library_dir, format!("runtimes/{rid}/native"));
            }
        }
    }

    /// **The Linux decision, as a test.** The stock build with the plugin is
    /// the default and needs nothing installed; the two CUDA archives are
    /// offered beside it on the same terms as on Windows; and every one of the
    /// three keeps a GPU provider for the inpainter, so no flavour puts it back
    /// on the CPU. Linux aarch64 has one build and it is the CPU.
    #[test]
    fn linux_x64_defaults_to_the_plugin_and_offers_cuda_beside_it() {
        let linux = Platform { os: Os::Linux, arch: Arch::X86_64 };
        assert_eq!(
            flavours(linux).iter().map(|p| p.flavour).collect::<Vec<_>>(),
            [Flavour::Stock, Flavour::Cuda12, Flavour::Cuda13],
            "the default must be first"
        );
        let default = for_platform(linux).expect("a default");
        assert_eq!(default.flavour, Flavour::Stock);
        assert!(default.user_installed.is_empty());
        assert!(default.has_gpu_provider());
        for package in flavours(linux) {
            assert!(package.advertised.contains(&"WebGpuExecutionProvider"), "{}", package.flavour.id());
        }
        for flavour in [Flavour::Cuda12, Flavour::Cuda13] {
            let package = for_flavour(linux, flavour).expect("a CUDA row");
            assert!(!package.default_flavour);
            assert!(package.advertised.contains(&"CUDAExecutionProvider"));
            assert_eq!(package.user_installed.len(), 2, "{}", flavour.id());
            assert_eq!(package.ort_version, "1.28.0");
        }

        let arm = Platform { os: Os::Linux, arch: Arch::Aarch64 };
        assert_eq!(flavours(arm).len(), 1);
        assert!(!for_platform(arm).unwrap().has_gpu_provider());
    }

    /// **The Windows decision, as a test.** DirectML is the default, it reaches
    /// every vendor through one provider, and it is the only Windows flavour
    /// with nothing for the user to install. CUDA is offered, is never a
    /// default, and says what it needs.
    #[test]
    fn windows_defaults_to_the_vendor_neutral_gpu_and_offers_cuda_beside_it() {
        let windows = Platform { os: Os::Windows, arch: Arch::X86_64 };
        let offered = flavours(windows);
        assert_eq!(
            offered.iter().map(|p| p.flavour).collect::<Vec<_>>(),
            [Flavour::DirectMl, Flavour::Cuda12, Flavour::Cuda13],
            "the default must be first"
        );

        let default = for_platform(windows).expect("a default");
        assert_eq!(default.flavour, Flavour::DirectMl);
        assert!(default.advertised.contains(&"DmlExecutionProvider"));
        assert!(default.user_installed.is_empty(), "the default must need nothing installed");

        for flavour in [Flavour::Cuda12, Flavour::Cuda13] {
            let package = for_flavour(windows, flavour).expect("a CUDA row");
            assert!(!package.default_flavour);
            assert!(package.advertised.contains(&"CUDAExecutionProvider"));
            // This archive has no DirectML in it, so choosing it *is* giving
            // up the provider that reaches every vendor. `accel`'s tests pin
            // where each model goes on it.
            assert!(!package.advertised.contains(&"DmlExecutionProvider"), "{}", flavour.id());
            // And the plugin is the fallback when CUDA is not usable.
            assert!(package.advertised.contains(&"WebGpuExecutionProvider"), "{}", flavour.id());
            assert_eq!(package.user_installed.len(), 2, "{}", flavour.id());
        }

        // Windows on ARM has no CUDA at all - NVIDIA publishes no toolkit for
        // it - so its only flavour is the one that works everywhere.
        let arm = Platform { os: Os::Windows, arch: Arch::Aarch64 };
        assert_eq!(flavours(arm).len(), 1);
        assert_eq!(for_platform(arm).unwrap().flavour, Flavour::DirectMl);
    }

    /// A flavour id survives a round trip, because it is what the setting and
    /// the downloader both store.
    #[test]
    fn a_flavour_is_named_by_a_stable_id() {
        for flavour in [Flavour::Stock, Flavour::DirectMl, Flavour::Cuda12, Flavour::Cuda13] {
            assert_eq!(Flavour::from_id(flavour.id()), Some(flavour));
        }
        assert_eq!(Flavour::from_id("rocm"), None);
    }

    /// Only CUDA asks anything of the user, and it says so in version strings
    /// rather than in catalogue keys - a toolkit version is data, like a licence
    /// identifier.
    #[test]
    fn only_the_cuda_flavours_need_something_installed_by_hand() {
        for package in PACKAGES {
            let cuda = matches!(package.flavour, Flavour::Cuda12 | Flavour::Cuda13);
            assert_eq!(
                !package.user_installed.is_empty(),
                cuda,
                "{:?} {}",
                package.platform,
                package.flavour.id()
            );
        }
    }

    #[test]
    fn the_gpu_path_differs_by_platform_and_the_table_says_so() {
        // The correction this module exists for, and its closure: macOS,
        // Windows and Linux x64 all have a GPU provider for the inpainter now,
        // and Linux aarch64 is the one platform left on the CPU.
        assert!(has_gpu_provider(Platform { os: Os::MacOs, arch: Arch::Aarch64 }));
        assert!(has_gpu_provider(Platform { os: Os::Windows, arch: Arch::X86_64 }));
        assert!(has_gpu_provider(Platform { os: Os::Linux, arch: Arch::X86_64 }));
        assert!(!has_gpu_provider(Platform { os: Os::Linux, arch: Arch::Aarch64 }));
    }

    #[test]
    fn every_advertised_provider_is_one_the_selector_knows() {
        let known: Vec<&str> =
            crate::accel::KNOWN.iter().map(|a| a.ort_name()).collect();
        for package in PACKAGES {
            for name in package.advertised {
                assert!(known.contains(name), "{name} is not a provider `accel` can select");
            }
        }
    }

    #[test]
    fn each_platform_has_exactly_one_default_and_each_flavour_appears_once() {
        for package in PACKAGES {
            let same_flavour = PACKAGES
                .iter()
                .filter(|p| p.platform == package.platform && p.flavour == package.flavour)
                .count();
            assert_eq!(same_flavour, 1, "{:?} {}", package.platform, package.flavour.id());

            let defaults = PACKAGES
                .iter()
                .filter(|p| p.platform == package.platform && p.default_flavour)
                .count();
            assert_eq!(defaults, 1, "{:?}", package.platform);
        }
    }

    #[test]
    fn this_machine_has_a_package() {
        // Intel macOS has no published 1.28 build, which is consistent with
        // arm64 being the first-release target - but this test runs
        // on the development machine, and there it must resolve.
        assert!(for_host().is_some(), "no package for {:?}", Platform::host());
    }
}
