//! Finding the CUDA toolkit and cuDNN that a CUDA build of ONNX Runtime needs,
//! and handing the right ones to it.
//!
//! The CUDA flavours ([`crate::runtime::package::Flavour::Cuda12`] and
//! `Cuda13`) carry `onnxruntime_providers_cuda` and nothing it depends on: the
//! CUDA runtime, cuBLAS, cuFFT and cuDNN 9 are the user's install. This module
//! is the whole of what the application knows about that install.
//!
//! ## Three ways the earlier check went wrong
//!
//! - **Any `cudart` counted.** The question asked was "is CUDA installed at
//!   all", so a CUDA 12 build on a machine with only CUDA 13 was told CUDA was
//!   there, picked CUDA automatically, and failed at session creation, every
//!   run, falling back to the CPU.
//! - **Only the loader path and `CUDA_PATH` were searched.** NVIDIA's cuDNN 9
//!   Windows installer puts its libraries under
//!   `C:\Program Files\NVIDIA\CUDNN\v9.x\bin\<cuda>\` and does not put that
//!   on `PATH`. A correct install read as missing, and CUDA was never offered.
//! - **Two toolkits side by side.** `CUDA_PATH` names the one installed last,
//!   and cuDNN publishes one `cudnn64_9.dll` per CUDA major under the same file
//!   name. Whichever was first on `PATH` was the one loaded, so a CUDA 13 cuDNN
//!   could land in a CUDA 12 process.
//!
//! ## What is done now
//!
//! Every install on the machine is listed with its CUDA major ([`Report`]).
//! The loaded build's own major is read off its path, the matching toolkit and
//! cuDNN are chosen ([`Report::verdict`]), and before the first CUDA session
//! their libraries are loaded **by absolute path** ([`prepare`]). A library
//! already in the process is the one the provider gets when it later asks for
//! it by name, so what `PATH` says no longer decides which install runs.
//!
//! cuDNN grows new parts between releases (`cudnn_engines_tensor_ir64_9.dll`
//! from 9.23) and opens them by name on its first convolution, not at load. So
//! every part in the chosen folder is loaded, not only the ones known here, and
//! on Windows both folders go on the process `PATH` for whatever else a library
//! asks for by name. A missing part does not show at load: the provider loads,
//! models with no convolution run, and every model with one fails.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// The CUDA libraries of one major version, as found on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Toolkit {
    pub major: u32,
    /// The minor release, where the install says it: the versioned file
    /// `libcudart.so.12.6.77` links to on Linux, the `v12.6` directory
    /// NVIDIA's Windows installer makes. `None` where neither says.
    pub minor: Option<u32>,
    /// Where its `cudart` is. The other libraries may sit elsewhere on Linux,
    /// where a distribution package splits them across directories.
    pub dir: PathBuf,
    /// Every library the provider loads, by absolute path, in load order.
    #[serde(skip)]
    pub libraries: Vec<PathBuf>,
    /// The file names of the ones that were not found anywhere.
    pub missing: Vec<String>,
}

/// One cuDNN 9 install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cudnn {
    /// The CUDA major it was built for, read off its directory
    /// (`bin\12.9`, `v12.6`, `cuda-13`). `None` where the path does not say,
    /// which is the usual Linux package; such an install matches any major.
    pub cuda_major: Option<u32>,
    pub dir: PathBuf,
    #[serde(skip)]
    pub libraries: Vec<PathBuf>,
    /// Parts in the folder that [`cudnn_libraries`] does not name: newer
    /// releases add some. Loaded where they will load, since an older part may
    /// be left over from another release.
    #[serde(skip)]
    pub extras: Vec<PathBuf>,
    pub missing: Vec<String>,
}

/// One NVIDIA adapter, as the driver names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    pub name: String,
    pub driver: String,
}

/// Everything this machine has for CUDA, and what the loaded build wants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// The CUDA major the loaded runtime was built against. `None` for a
    /// runtime that is not a CUDA build, or one whose path does not say.
    pub wanted: Option<u32>,
    pub toolkits: Vec<Toolkit>,
    pub cudnn: Vec<Cudnn>,
    pub gpus: Vec<Gpu>,
}

/// Whether the loaded build can run CUDA here, and the remedy when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ready { toolkit: Toolkit, cudnn: Cudnn },
    /// The driver reports no NVIDIA adapter, or there is no driver.
    NoGpu,
    /// The driver is older than the build's CUDA major needs.
    DriverTooOld { have: u32, need: u32 },
    /// The wanted major is installed, but every complete install of it is a
    /// minor release older than the build links against (`12.6`, `12.8`).
    ToolkitTooOld { have: String, need: String },
    /// No complete toolkit of the wanted major.
    NoToolkit,
    /// No complete cuDNN 9 for the wanted major.
    NoCudnn,
}

impl Verdict {
    /// The i18n key the interface names it under.
    pub fn key(&self) -> &'static str {
        match self {
            Verdict::Ready { .. } => "accel.cudaCheck.ready",
            Verdict::NoGpu => "accel.cudaCheck.noGpu",
            Verdict::DriverTooOld { .. } => "accel.cudaCheck.driverTooOld",
            Verdict::ToolkitTooOld { .. } => "accel.cudaCheck.toolkitTooOld",
            Verdict::NoToolkit => "accel.cudaCheck.noToolkit",
            Verdict::NoCudnn => "accel.cudaCheck.noCudnn",
        }
    }
}

/// The oldest driver major each CUDA major runs on: 525 for CUDA 12 (the
/// 12.0 release notes: 525.60.13 on Linux, 527.41 on Windows) and 580 for
/// CUDA 13.
fn driver_floor(cuda_major: u32) -> Option<u32> {
    match cuda_major {
        12 => Some(525),
        13 => Some(580),
        _ => None,
    }
}

/// The oldest minor release of each CUDA major the app's builds run on. ONNX
/// Runtime 1.28's CUDA 12 build calls `cudaLibraryLoadData` and three more
/// functions the CUDA 12.6 runtime does not export; NVIDIA released no 12.7,
/// so 12.8 is the floor. Found by `.github/workflows/probe-cuda.yml`, which
/// loads the build against an older toolkit; move it with the runtime version.
fn minor_floor(cuda_major: u32) -> Option<u32> {
    match cuda_major {
        12 => Some(8),
        _ => None,
    }
}

impl Toolkit {
    fn version(&self) -> String {
        match self.minor {
            Some(minor) => format!("{}.{minor}", self.major),
            None => self.major.to_string(),
        }
    }

    /// New enough for a build of its major. A minor release that cannot be
    /// read is let through: refusing it would refuse a working install, and
    /// the session itself still reports a real mismatch.
    fn new_enough(&self) -> bool {
        match (self.minor, minor_floor(self.major)) {
            (Some(minor), Some(floor)) => minor >= floor,
            _ => true,
        }
    }
}

impl Report {
    pub fn verdict(&self) -> Verdict {
        if self.gpus.is_empty() {
            return Verdict::NoGpu;
        }
        if let Some(need) = self.wanted.and_then(driver_floor) {
            let have = self.gpus.iter().filter_map(|gpu| driver_major(&gpu.driver)).max();
            if let Some(have) = have.filter(|have| *have < need) {
                return Verdict::DriverTooOld { have, need };
            }
        }
        let mut complete = self
            .toolkits
            .iter()
            .filter(|toolkit| toolkit.missing.is_empty())
            .filter(|toolkit| self.wanted.is_none_or(|wanted| toolkit.major == wanted))
            .peekable();
        // The newest is first (`toolkits_in` sorts them), so it is the one to
        // name when none is new enough.
        let Some(newest) = complete.peek().copied() else { return Verdict::NoToolkit };
        let Some(toolkit) = complete.find(|toolkit| toolkit.new_enough()) else {
            let need = format!("{}.{}", newest.major, minor_floor(newest.major).unwrap_or(0));
            return Verdict::ToolkitTooOld { have: newest.version(), need };
        };
        // A build of unknown CUDA (a runtime placed by hand) is held to the
        // floor of the toolkit it would use.
        if self.wanted.is_none() {
            let have = self.gpus.iter().filter_map(|gpu| driver_major(&gpu.driver)).max();
            if let (Some(need), Some(have)) = (driver_floor(toolkit.major), have) {
                if have < need {
                    return Verdict::DriverTooOld { have, need };
                }
            }
        }
        // A cuDNN that names the toolkit's major beats one that names none.
        let cudnn = self
            .cudnn
            .iter()
            .filter(|cudnn| cudnn.missing.is_empty())
            .filter(|cudnn| cudnn.cuda_major.is_none_or(|major| major == toolkit.major))
            .min_by_key(|cudnn| cudnn.cuda_major.is_none());
        match cudnn {
            Some(cudnn) => Verdict::Ready { toolkit: toolkit.clone(), cudnn: cudnn.clone() },
            None => Verdict::NoCudnn,
        }
    }
}

static REPORT: Mutex<Option<Report>> = Mutex::new(None);

/// The machine's CUDA install, read once and kept. [`rescan`] forgets it, so
/// a toolkit installed while the application is open is found on the next
/// check without a restart.
pub fn report() -> Report {
    let mut cached = REPORT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cached.get_or_insert_with(read_machine).clone()
}

pub fn rescan() {
    *REPORT.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    crate::accel::forget_proofs();
}

fn read_machine() -> Report {
    let wanted = loaded_cuda_major();
    if !cfg!(any(target_os = "windows", target_os = "linux")) {
        // NVIDIA publishes no CUDA for macOS, and no runtime here carries it.
        return Report { wanted, toolkits: Vec::new(), cudnn: Vec::new(), gpus: Vec::new() };
    }
    let dirs = candidate_dirs();
    Report { wanted, toolkits: toolkits_in(&dirs), cudnn: cudnn_in(&dirs), gpus: nvidia_gpus() }
}

/// Load the chosen toolkit and cuDNN into this process, once, before the
/// first CUDA session. An error is the reason no CUDA session can be built.
pub fn prepare() -> Result<(), String> {
    static PREPARED: Mutex<bool> = Mutex::new(false);
    let mut prepared = PREPARED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if *prepared {
        return Ok(());
    }
    let Verdict::Ready { toolkit, cudnn } = report().verdict() else {
        return Err("CUDA is not ready on this machine; see Settings, Runtime".to_owned());
    };
    search_folders(&[&toolkit.dir, &cudnn.dir]);
    for library in &toolkit.libraries {
        ort::util::preload_dylib(library).map_err(|e| format!("could not load {}: {e}", library.display()))?;
    }
    // Its other parts before `cudnn64_9`, which is last in the list.
    let (main, parts) = cudnn.libraries.split_last().ok_or("cuDNN has no libraries")?;
    for library in parts {
        ort::util::preload_dylib(library).map_err(|e| format!("could not load {}: {e}", library.display()))?;
    }
    for library in &cudnn.extras {
        let _ = ort::util::preload_dylib(library);
    }
    ort::util::preload_dylib(main).map_err(|e| format!("could not load {}: {e}", main.display()))?;
    *prepared = true;
    Ok(())
}

/// Put these folders first on this process's `PATH`, where `LoadLibrary` looks
/// for a name no loaded module answers to. `SetDllDirectoryW` holds one folder
/// and the runtime's has it ([`crate::runtime`]). Linux reads its loader path
/// once at start, so this would do nothing there.
fn search_folders(folders: &[&Path]) {
    if !cfg!(target_os = "windows") {
        return;
    }
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = folders.iter().map(|folder| folder.to_path_buf()).collect();
    paths.extend(std::env::split_paths(&current).filter(|path| !folders.contains(&path.as_path())));
    if let Ok(joined) = std::env::join_paths(paths) {
        // Windows' environment calls take their own lock, so this is sound
        // beside other threads there, and it runs nowhere else.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

/// The CUDA major the loaded runtime was built against. Nothing the runtime
/// reports through its API says which CUDA it links, and the app's packages
/// all unpack to the same file name, so it is the install record beside the
/// library first ([`crate::runtime::loaded_build`]), and the path only for a
/// runtime placed by hand under its archive's name
/// (`onnxruntime-win-x64-gpu_cuda12-1.28.0/lib`).
fn loaded_cuda_major() -> Option<u32> {
    crate::runtime::loaded_build()
        .and_then(|build| cuda_major_of_runtime(Path::new(&build.flavour)))
        .or_else(|| crate::runtime::loaded().and_then(cuda_major_of_runtime))
}

/// The CUDA major a build name or a library path names: `cuda12`, `cuda13`.
pub fn cuda_major_of_runtime(path: &Path) -> Option<u32> {
    let text = path.to_string_lossy().to_ascii_lowercase();
    ["cuda13", "cuda12"].into_iter().find(|tag| text.contains(tag)).and_then(|tag| tag[4..].parse().ok())
}

/// The libraries `onnxruntime_providers_cuda` loads from a toolkit, in load
/// order: a dependency before its dependents. cuFFT's major is one behind
/// CUDA's in both releases offered.
fn toolkit_libraries(major: u32) -> Vec<String> {
    let fft = major.saturating_sub(1);
    if cfg!(target_os = "windows") {
        vec![
            format!("cudart64_{major}.dll"),
            format!("cublasLt64_{major}.dll"),
            format!("cublas64_{major}.dll"),
            "curand64_10.dll".to_owned(),
            format!("cufft64_{fft}.dll"),
        ]
    } else {
        vec![
            format!("libcudart.so.{major}"),
            format!("libcublasLt.so.{major}"),
            format!("libcublas.so.{major}"),
            "libcurand.so.10".to_owned(),
            format!("libcufft.so.{fft}"),
        ]
    }
}

/// cuDNN 9's libraries. `cudnn64_9` loads the others by name at run time, so
/// they are loaded first and it finds them in the process.
fn cudnn_libraries() -> Vec<String> {
    let parts = ["graph", "ops", "heuristic", "adv", "cnn", "engines_precompiled", "engines_runtime_compiled"];
    if cfg!(target_os = "windows") {
        parts.iter().map(|part| format!("cudnn_{part}64_9.dll")).chain(["cudnn64_9.dll".to_owned()]).collect()
    } else {
        parts.iter().map(|part| format!("libcudnn_{part}.so.9")).chain(["libcudnn.so.9".to_owned()]).collect()
    }
}

/// The CUDA major a `cudart` file name is for: `cudart64_12.dll`,
/// `libcudart.so.13`, `libcudart.so.12.4.127`.
fn cudart_major(name: &str) -> Option<u32> {
    let name = name.to_ascii_lowercase();
    let digits = if let Some(rest) = name.strip_prefix("cudart64_") {
        rest.strip_suffix(".dll")?
    } else {
        name.strip_prefix("libcudart.so.")?.split('.').next()?
    };
    // CUDA 11 named its runtime `cudart64_110.dll`. Two digits from 20 up are
    // older still: PhysX ships CUDA 6.5's `cudart64_65.dll` on every NVIDIA
    // machine, which is no toolkit of any build here.
    let major: u32 = digits.parse().ok()?;
    match major {
        100.. => Some(major / 10),
        20.. => None,
        _ => Some(major),
    }
}

fn is_cudnn_main(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "cudnn64_9.dll" || name == "libcudnn.so.9" || name.starts_with("libcudnn.so.9.")
}

/// A library in a directory by its soname, or by a fuller version of it where
/// only that is on disk (`libcudart.so.12.4.127` without the link).
fn find_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    if cfg!(target_os = "windows") {
        return None;
    }
    let prefix = format!("{name}.");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
}

/// Every CUDA major with a `cudart` somewhere in `dirs`, its other libraries
/// looked for beside it first and then anywhere else in `dirs`.
pub fn toolkits_in(dirs: &[PathBuf]) -> Vec<Toolkit> {
    let mut found: Vec<(u32, PathBuf, PathBuf)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            if let Some(major) = cudart_major(&entry.file_name().to_string_lossy()) {
                if !found.iter().any(|(m, d, _)| *m == major && d == dir) {
                    found.push((major, dir.clone(), entry.path()));
                }
            }
        }
    }
    let mut toolkits: Vec<Toolkit> = found
        .into_iter()
        .map(|(major, dir, cudart)| {
            let (libraries, missing) = resolve(&toolkit_libraries(major), &dir, dirs);
            let minor = cudart_minor(&cudart, major).or_else(|| match cuda_version_hint(&dir) {
                Some((hinted, minor)) if hinted == major => minor,
                _ => None,
            });
            Toolkit { major, minor, dir, libraries, missing }
        })
        .collect();
    // A complete install before a partial one, then the newer release.
    toolkits.sort_by_key(|toolkit| {
        (!toolkit.missing.is_empty(), std::cmp::Reverse(toolkit.major), std::cmp::Reverse(toolkit.minor))
    });
    toolkits
}

/// Every cuDNN 9 in `dirs`. Its parts ship together, so they are looked for
/// in its own directory only.
pub fn cudnn_in(dirs: &[PathBuf]) -> Vec<Cudnn> {
    let mut installs = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        if !entries.flatten().any(|entry| is_cudnn_main(&entry.file_name().to_string_lossy())) {
            continue;
        }
        let (libraries, missing) = resolve(&cudnn_libraries(), dir, std::slice::from_ref(dir));
        let extras = cudnn_extras(dir, &libraries);
        // `/usr/local/cuda` is a link to `/usr/local/cuda-12.6`: the name the
        // link points at says the major the link's own name does not.
        let cuda_major = cuda_major_hint(dir).or_else(|| std::fs::canonicalize(dir).ok().and_then(|real| cuda_major_hint(&real)));
        installs.push(Cudnn { cuda_major, dir: dir.clone(), libraries, extras, missing });
    }
    installs
}

/// Every other cuDNN 9 part in `dir`, in name order so a load order repeats.
fn cudnn_extras(dir: &Path, known: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut extras: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            let part = if cfg!(target_os = "windows") {
                name.starts_with("cudnn_") && name.ends_with("64_9.dll")
            } else {
                name.starts_with("libcudnn_") && name.ends_with(".so.9")
            };
            part && !known.iter().any(|library| library.file_name() == Some(&entry.file_name()))
        })
        .map(|entry| entry.path())
        .collect();
    extras.sort();
    extras
}

fn resolve(names: &[String], home: &Path, dirs: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut libraries = Vec::new();
    let mut missing = Vec::new();
    for name in names {
        let path = find_in(home, name).or_else(|| dirs.iter().find_map(|dir| find_in(dir, name)));
        match path {
            Some(path) => libraries.push(path),
            None => missing.push(name.clone()),
        }
    }
    (libraries, missing)
}

/// The CUDA major a directory path names, nearest component first: `12.9`
/// and `v12.6` as NVIDIA's Windows installers lay them out, `cuda-13.0` and
/// `cuda12` as Linux ones do.
pub fn cuda_major_hint(dir: &Path) -> Option<u32> {
    cuda_version_hint(dir).map(|(major, _)| major)
}

/// [`cuda_major_hint`], and the minor release where the path names one
/// (`v12.6`, not `cuda12`).
fn cuda_version_hint(dir: &Path) -> Option<(u32, Option<u32>)> {
    // Split by hand rather than by `components`, so a Windows path reads the
    // same in a test run on any platform.
    let text = dir.to_string_lossy().to_ascii_lowercase();
    text.rsplit(['/', '\\']).find_map(|part| {
        let part = part.strip_prefix("cuda").map(|rest| rest.trim_start_matches(['-', '_'])).unwrap_or(part);
        let part = part.strip_prefix('v').unwrap_or(part);
        let (major, minor) = match part.split_once('.') {
            Some((major, minor)) => (major, Some(minor.parse::<u32>().ok()?)),
            None => (part, None),
        };
        let major: u32 = major.parse().ok()?;
        (10..=20).contains(&major).then_some((major, minor))
    })
}

/// The minor release of a Linux `cudart`, from the versioned file its soname
/// links to: `libcudart.so.12` is `libcudart.so.12.6.77`. Windows names the
/// DLL by major only, and a pip wheel ships the soname as the file itself.
fn cudart_minor(cudart: &Path, major: u32) -> Option<u32> {
    let real = std::fs::canonicalize(cudart).ok()?;
    let name = real.file_name()?.to_string_lossy().into_owned();
    let rest = name.strip_prefix(&format!("libcudart.so.{major}."))?;
    rest.split('.').next()?.parse().ok()
}

fn driver_major(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok()
}

/// Every NVIDIA adapter the driver reports. `nvidia-smi` is installed with
/// the driver on both platforms (`System32` on Windows), so its absence is a
/// machine with no NVIDIA driver.
fn nvidia_gpus() -> Vec<Gpu> {
    let output = crate::process::command("nvidia-smi")
        .args(["--query-gpu=name,driver_version", "--format=csv,noheader"])
        .output();
    match output {
        Ok(output) if output.status.success() => parse_gpus(&String::from_utf8_lossy(&output.stdout)),
        _ => Vec::new(),
    }
}

fn parse_gpus(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|line| {
            let (name, driver) = line.rsplit_once(',')?;
            let (name, driver) = (name.trim(), driver.trim());
            (!name.is_empty() && driver_major(driver).is_some())
                .then(|| Gpu { name: name.to_owned(), driver: driver.to_owned() })
        })
        .collect()
}

/// Every directory a toolkit or cuDNN could be in on this machine.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let loader_path = if cfg!(target_os = "windows") { "PATH" } else { "LD_LIBRARY_PATH" };
    if let Some(value) = std::env::var_os(loader_path) {
        dirs.extend(std::env::split_paths(&value));
    }
    if cfg!(target_os = "windows") {
        // `CUDA_PATH` is the toolkit installed last; `CUDA_PATH_V12_6` and the
        // like are every toolkit, one variable each.
        let roots = std::env::vars_os()
            .filter(|(name, _)| name.to_string_lossy().to_ascii_uppercase().starts_with("CUDA_PATH"))
            .map(|(_, value)| PathBuf::from(value));
        for root in roots.chain(children(&program_files().join(r"NVIDIA GPU Computing Toolkit\CUDA"))) {
            // CUDA 13 moved its libraries into `bin\x64`.
            dirs.push(root.join("bin"));
            dirs.push(root.join(r"bin\x64"));
        }
        let cudnn_roots = ["CUDNN_PATH", "CUDNN_HOME"].into_iter().filter_map(std::env::var_os).map(PathBuf::from);
        for root in cudnn_roots.chain(children(&program_files().join(r"NVIDIA\CUDNN"))) {
            let bin = root.join("bin");
            dirs.push(root.clone());
            dirs.push(bin.clone());
            for per_cuda in children(&bin) {
                dirs.push(per_cuda.join("x64"));
                dirs.push(per_cuda);
            }
        }
    } else {
        for name in ["CUDA_PATH", "CUDA_HOME"] {
            if let Some(root) = std::env::var_os(name) {
                dirs.push(PathBuf::from(root).join("lib64"));
            }
        }
        // `/usr/local/cuda` is the link a normal install leaves and
        // `/usr/local/cuda-12.6` what it points at; a machine with two
        // toolkits has both versioned ones.
        for root in children(Path::new("/usr/local")) {
            if root.file_name().is_some_and(|name| name.to_string_lossy().starts_with("cuda")) {
                dirs.push(root.join("lib64"));
                dirs.push(root.join("targets/x86_64-linux/lib"));
            }
        }
        dirs.extend(["/usr/lib/x86_64-linux-gnu", "/usr/lib64", "/usr/lib"].map(PathBuf::from));
    }
    // One entry per real directory: `/usr/local/cuda`, `cuda-13` and
    // `cuda-13.0/lib64` are often the same one, and each would be listed as
    // another install. The first spelling found is kept.
    let mut unique = Vec::new();
    let mut seen = Vec::new();
    for dir in dirs {
        let real = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !seen.contains(&real) {
            seen.push(real);
            unique.push(dir);
        }
    }
    unique
}

fn program_files() -> PathBuf {
    std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

fn children(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("manga-cleaner-cuda-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &Path, names: &[String]) {
        std::fs::create_dir_all(dir).unwrap();
        for name in names {
            std::fs::write(dir.join(name), []).unwrap();
        }
    }

    fn gpu(driver: &str) -> Gpu {
        Gpu { name: "NVIDIA GeForce RTX 4070".to_owned(), driver: driver.to_owned() }
    }

    #[test]
    fn the_runtime_path_names_its_cuda_major() {
        let windows = Path::new(r"C:\runtimes\cuda12\onnxruntime-win-x64-gpu_cuda12-1.28.0\lib\onnxruntime.dll");
        assert_eq!(cuda_major_of_runtime(windows), Some(12));
        let linux = Path::new("/data/runtimes/onnxruntime-linux-x64-gpu_cuda13-1.28.0/lib/libonnxruntime.so");
        assert_eq!(cuda_major_of_runtime(linux), Some(13));
        assert_eq!(cuda_major_of_runtime(Path::new("/r/onnxruntime-win-x64-1.28.0/lib/onnxruntime.dll")), None);
    }

    #[test]
    fn cudart_names_carry_their_major() {
        assert_eq!(cudart_major("cudart64_12.dll"), Some(12));
        assert_eq!(cudart_major("CUDART64_13.DLL"), Some(13));
        assert_eq!(cudart_major("cudart64_110.dll"), Some(11));
        assert_eq!(cudart_major("libcudart.so.12"), Some(12));
        assert_eq!(cudart_major("libcudart.so.13.0.48"), Some(13));
        assert_eq!(cudart_major("libcudart.so"), None);
        assert_eq!(cudart_major("cudart64_12.lib"), None);
        assert_eq!(cudart_major("cudnn64_9.dll"), None);
        // PhysX's CUDA 6.5, on every NVIDIA machine, is no toolkit.
        assert_eq!(cudart_major("cudart64_65.dll"), None);
    }

    /// A newer cuDNN's extra part is found by looking, with no list to update.
    #[test]
    fn every_cudnn_part_in_the_folder_is_loaded() {
        let dir = scratch("extras");
        let (extra, other) = if cfg!(target_os = "windows") {
            ("cudnn_engines_tensor_ir64_9.dll", "cudnn_notes.txt")
        } else {
            ("libcudnn_engines_tensor_ir.so.9", "libcudnn_notes.txt")
        };
        touch(&dir, &cudnn_libraries());
        touch(&dir, &[extra.to_owned(), other.to_owned()]);
        let found = cudnn_in(std::slice::from_ref(&dir));
        assert_eq!(found[0].extras, vec![dir.join(extra)]);
        assert!(found[0].missing.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A machine that is ready, as `prepare` and `setup` read it: every library
    /// the one named, and the cuDNN in `cudnn_dir`.
    fn ready(library: &Path, cudnn_dir: &str) -> Report {
        Report {
            wanted: Some(12),
            toolkits: vec![Toolkit {
                major: 12,
                minor: Some(9),
                dir: PathBuf::from("/toolkit/v12.9/bin"),
                libraries: vec![library.to_path_buf()],
                missing: Vec::new(),
            }],
            cudnn: vec![Cudnn {
                cuda_major: Some(12),
                dir: PathBuf::from(cudnn_dir),
                libraries: vec![library.to_path_buf()],
                extras: Vec::new(),
                missing: Vec::new(),
            }],
            gpus: vec![gpu("566.36")],
        }
    }

    /// `prepare` latches, so an install the user fixes and rechecks is never
    /// loaded: the proof runs again against the old libraries and fails again,
    /// and nothing tells the user a restart is what it takes. Here the second
    /// install's library does not exist, so loading it would be an error.
    #[test]
    #[ignore = "BUG in 46429f8: Check again cannot load a new install, and no restart is asked for"]
    fn check_again_loads_the_install_it_found() {
        let system = Path::new(if cfg!(windows) {
            "kernel32.dll"
        } else if cfg!(target_os = "macos") {
            "/usr/lib/libz.1.dylib"
        } else {
            "libz.so.1"
        });
        *REPORT.lock().unwrap() = Some(ready(system, "/cudnn/v9.13/bin/12.9"));
        prepare().expect("the first install loads");
        *REPORT.lock().unwrap() = Some(ready(Path::new("/no/such/library"), "/cudnn/v9.23/bin/12.9"));
        assert!(prepare().is_err(), "the second install was never loaded");
    }

    /// `setup` names the driver and not the toolkit or cuDNN, so a measurement
    /// taken while CUDA failed stays current after the install is fixed, and
    /// the same goes for one saved before 46429f8 (`VERSION` did not move).
    /// Automatic keeps following its CPU winner, and only a new Measure tells.
    /// `RT_RUNTIME` the runtime library.
    #[test]
    #[ignore = "BUG in 46429f8: a measurement of a broken CUDA stays in force once fixed; needs RT_RUNTIME"]
    fn a_new_cudnn_is_a_new_setup() {
        let runtime = std::env::var("RT_RUNTIME").expect("RT_RUNTIME");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let library = Path::new("/unused");
        *REPORT.lock().unwrap() = Some(ready(library, "/cudnn/v9.13/bin/12.9"));
        let before = crate::measure::setup();
        *REPORT.lock().unwrap() = Some(ready(library, "/cudnn/v9.23/bin/12.9"));
        assert_ne!(before, crate::measure::setup());
    }

    /// `find_in` takes a known part by its fuller version where only that is on
    /// disk, and `cudnn_extras` does not, so an unknown part with no `.so.9`
    /// link is the very file this commit set out to load, left unloaded.
    #[cfg(unix)]
    #[test]
    #[ignore = "BUG in 46429f8: a part with only its full version on disk is not an extra"]
    fn a_cudnn_part_with_only_its_full_version_on_disk_is_loaded() {
        let dir = scratch("extras-versioned");
        let extra = "libcudnn_engines_tensor_ir.so.9.23.1";
        touch(&dir, &cudnn_libraries());
        touch(&dir, &[extra.to_owned()]);
        let found = cudnn_in(std::slice::from_ref(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found[0].extras, vec![dir.join(extra)]);
    }

    #[test]
    fn a_cudnn_directory_says_which_cuda_it_is_for() {
        assert_eq!(cuda_major_hint(Path::new(r"C:\Program Files\NVIDIA\CUDNN\v9.8\bin\12.8")), Some(12));
        assert_eq!(cuda_major_hint(Path::new(r"C:\Program Files\NVIDIA\CUDNN\v9.14\bin\13.0\x64")), Some(13));
        assert_eq!(cuda_major_hint(Path::new(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.6\bin")), Some(12));
        assert_eq!(cuda_major_hint(Path::new("/usr/local/cuda-13.0/lib64")), Some(13));
        // `v9.8` is cuDNN's own version, not a CUDA one, and the Linux
        // package directory says nothing.
        assert_eq!(cuda_major_hint(Path::new(r"C:\Program Files\NVIDIA\CUDNN\v9.8\bin")), None);
        assert_eq!(cuda_major_hint(Path::new("/usr/lib/x86_64-linux-gnu")), None);
    }

    /// Two toolkits and two cuDNN builds on one machine, the case that used to
    /// load whichever was first on the path. A CUDA 12 build takes the CUDA 12
    /// pair and nothing of the other.
    #[test]
    fn two_toolkits_side_by_side_resolve_to_the_builds_own_major() {
        let root = scratch("side-by-side");
        let (v12, v13) = (root.join("v12.9/bin"), root.join("v13.0/bin"));
        touch(&v12, &toolkit_libraries(12));
        touch(&v13, &toolkit_libraries(13));
        let (dnn12, dnn13) = (root.join("CUDNN/v9.8/bin/12.8"), root.join("CUDNN/v9.8/bin/13.0"));
        touch(&dnn12, &cudnn_libraries());
        touch(&dnn13, &cudnn_libraries());
        let dirs = vec![v13.clone(), dnn13.clone(), v12.clone(), dnn12.clone()];

        let report = |wanted| Report {
            wanted: Some(wanted),
            toolkits: toolkits_in(&dirs),
            cudnn: cudnn_in(&dirs),
            gpus: vec![gpu("581.42")],
        };
        let Verdict::Ready { toolkit, cudnn } = report(12).verdict() else { panic!("CUDA 12 is installed") };
        assert_eq!((toolkit.major, toolkit.dir), (12, v12.clone()));
        assert_eq!(cudnn.dir, dnn12);
        assert!(toolkit.libraries.iter().all(|path| path.starts_with(&v12)));
        let Verdict::Ready { toolkit, cudnn } = report(13).verdict() else { panic!("CUDA 13 is installed") };
        assert_eq!((toolkit.major, cudnn.dir), (13, dnn13));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_toolkit_of_the_wrong_major_is_not_the_one_wanted() {
        let root = scratch("wrong-major");
        let v13 = root.join("v13.0/bin");
        touch(&v13, &toolkit_libraries(13));
        touch(&v13, &cudnn_libraries());
        let dirs = vec![v13];
        let report = Report { wanted: Some(12), toolkits: toolkits_in(&dirs), cudnn: cudnn_in(&dirs), gpus: vec![gpu("566.36")] };
        assert_eq!(report.verdict(), Verdict::NoToolkit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_cudnn_built_for_another_cuda_is_no_cudnn() {
        let root = scratch("wrong-cudnn");
        let (v12, dnn13) = (root.join("v12.9/bin"), root.join("CUDNN/v9.8/bin/13.0"));
        touch(&v12, &toolkit_libraries(12));
        touch(&dnn13, &cudnn_libraries());
        let dirs = vec![v12, dnn13];
        let report = Report { wanted: Some(12), toolkits: toolkits_in(&dirs), cudnn: cudnn_in(&dirs), gpus: vec![gpu("566.36")] };
        assert_eq!(report.verdict(), Verdict::NoCudnn);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_partial_toolkit_names_what_it_lacks() {
        let root = scratch("partial");
        let bin = root.join("v12.6/bin");
        touch(&bin, &toolkit_libraries(12)[..1]);
        let toolkits = toolkits_in(std::slice::from_ref(&bin));
        assert_eq!(toolkits.len(), 1);
        assert_eq!(toolkits[0].missing, toolkit_libraries(12)[1..].to_vec());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn no_adapter_and_an_old_driver_come_before_the_install() {
        let mut report = Report { wanted: Some(13), toolkits: Vec::new(), cudnn: Vec::new(), gpus: Vec::new() };
        assert_eq!(report.verdict(), Verdict::NoGpu);
        report.gpus = vec![gpu("566.36")];
        assert_eq!(report.verdict(), Verdict::DriverTooOld { have: 566, need: 580 });
        report.wanted = Some(12);
        assert_eq!(report.verdict(), Verdict::NoToolkit);
    }

    /// The app's packages all unpack to `runtimes/onnxruntime.dll`, so the
    /// path never names the build: the install record beside it does.
    #[test]
    fn the_install_record_names_the_build_the_path_does_not() {
        let root = scratch("install-record");
        let library = root.join(if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" });
        assert_eq!(cuda_major_of_runtime(&library), None);
        std::fs::write(root.join(crate::runtime::INSTALLED_FILE), br#"{"flavour":"cuda12","version":"1.28.0","files":[]}"#).unwrap();
        let build = crate::runtime::build_of(&library).expect("the record is read");
        assert_eq!((build.flavour.as_str(), build.version.as_str()), ("cuda12", "1.28.0"));
        assert_eq!(cuda_major_of_runtime(Path::new(&build.flavour)), Some(12));
        std::fs::write(root.join(crate::runtime::INSTALLED_FILE), b"not json").unwrap();
        assert_eq!(crate::runtime::build_of(&library), None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A runtime placed by hand says no CUDA major; the toolkit it would use
    /// still has a driver floor.
    #[test]
    fn a_build_of_unknown_cuda_is_held_to_its_toolkits_driver_floor() {
        let root = scratch("unknown-major");
        let (v13, dnn13) = (root.join("v13.0/bin"), root.join("CUDNN/v9.14/bin/13.0"));
        touch(&v13, &toolkit_libraries(13));
        touch(&dnn13, &cudnn_libraries());
        let dirs = vec![v13, dnn13];
        let mut report = Report { wanted: None, toolkits: toolkits_in(&dirs), cudnn: cudnn_in(&dirs), gpus: vec![gpu("560.94")] };
        assert_eq!(report.verdict(), Verdict::DriverTooOld { have: 560, need: 580 });
        report.gpus = vec![gpu("581.42")];
        assert!(matches!(report.verdict(), Verdict::Ready { .. }));
        let _ = std::fs::remove_dir_all(root);
    }

    /// CUDA 12.6 lacks functions the CUDA 12 build calls, so a machine with
    /// only 12.6 is told to update, and one with 12.6 and 12.9 gets 12.9
    /// whatever the order on the path.
    #[test]
    fn a_cuda_12_older_than_the_build_is_too_old_and_the_newest_is_chosen() {
        let root = scratch("minor");
        let (old, new) = (root.join("v12.6/bin"), root.join("v12.9/bin"));
        touch(&old, &toolkit_libraries(12));
        touch(&old, &cudnn_libraries());
        let report = |dirs: &[PathBuf]| Report {
            wanted: Some(12),
            toolkits: toolkits_in(dirs),
            cudnn: cudnn_in(dirs),
            gpus: vec![gpu("566.36")],
        };
        assert_eq!(
            report(std::slice::from_ref(&old)).verdict(),
            Verdict::ToolkitTooOld { have: "12.6".to_owned(), need: "12.8".to_owned() }
        );
        touch(&new, &toolkit_libraries(12));
        let Verdict::Ready { toolkit, .. } = report(&[old, new.clone()]).verdict() else { panic!("CUDA 12.9 is installed") };
        assert_eq!((toolkit.minor, toolkit.dir), (Some(9), new));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A directory that names no minor release is not refused for it.
    #[test]
    fn a_cuda_12_of_unknown_minor_is_let_through() {
        let root = scratch("no-minor");
        let lib = root.join("site-packages/nvidia/lib");
        touch(&lib, &toolkit_libraries(12));
        touch(&lib, &cudnn_libraries());
        let dirs = vec![lib];
        let report = Report { wanted: Some(12), toolkits: toolkits_in(&dirs), cudnn: cudnn_in(&dirs), gpus: vec![gpu("566.36")] };
        assert!(matches!(report.verdict(), Verdict::Ready { toolkit: Toolkit { minor: None, .. }, .. }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_path_names_its_minor_release_where_it_has_one() {
        assert_eq!(cuda_version_hint(Path::new(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.6\bin")), Some((12, Some(6))));
        assert_eq!(cuda_version_hint(Path::new("/usr/local/cuda-12.9/targets/x86_64-linux/lib")), Some((12, Some(9))));
        assert_eq!(cuda_version_hint(Path::new("/opt/cuda12/lib64")), Some((12, None)));
    }

    /// Linux names the minor release in the file `libcudart.so.12` links to.
    #[cfg(unix)]
    #[test]
    fn a_linux_cudart_reads_its_minor_from_the_file_it_links_to() {
        let root = scratch("cudart-link");
        let lib = root.join("lib");
        touch(&lib, &["libcudart.so.12.8.90".to_owned()]);
        std::os::unix::fs::symlink("libcudart.so.12.8.90", lib.join("libcudart.so.12")).unwrap();
        assert_eq!(cudart_minor(&lib.join("libcudart.so.12"), 12), Some(8));
        assert_eq!(cudart_minor(&lib.join("libcudart.so.12.8.90"), 13), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_cudnn_behind_a_link_reads_the_major_of_what_it_links_to() {
        let root = scratch("linked");
        let real = root.join("cuda-12.6/lib64");
        touch(&real, &cudnn_libraries());
        std::os::unix::fs::symlink(root.join("cuda-12.6"), root.join("cuda")).unwrap();
        let installs = cudnn_in(&[root.join("cuda/lib64")]);
        assert_eq!(installs[0].cuda_major, Some(12));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_driver_query_is_read_line_by_line() {
        let gpus = parse_gpus("NVIDIA GeForce RTX 3060, 566.36\nNVIDIA RTX A2000, Laptop, 552.22\n\ngarbage\n");
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[1].name, "NVIDIA RTX A2000, Laptop");
        assert_eq!(gpus[1].driver, "552.22");
    }
}
