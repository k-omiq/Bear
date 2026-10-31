//! Finding and loading ONNX Runtime.
//!
//! The runtime is not in the bundle: a small signed application downloads it
//! on first launch and `dlopen`s it. Phase 0 spike 1 measured what that
//! costs and what breaks it, and both findings live here rather than in a
//! comment on the download.
//!
//! Two of them shape this module:
//!
//! - The application must carry `com.apple.security.cs.disable-library-validation`,
//!   because every official ONNX Runtime release is ad-hoc signed with no Team
//!   Identifier and a hardened-runtime process refuses to map it otherwise.
//! - Which archive a platform needs, and which execution providers it turns out
//!   to carry, is [`package`] - they are not the same across platforms and the
//!   difference decides whether the inpainter can leave the CPU.
//! - A runtime carrying `com.apple.quarantine` is refused by policy, and the
//!   operating system's message for that is nothing like the message for a
//!   missing file. [`LoadError`] separates the two so the interface can say
//!   which one happened.
//! - **The Windows runtime is not self-contained either**, and this is not a
//!   signing problem. The shipped `onnxruntime.dll` imports the Visual C++
//!   runtime as a regular import, which is a component the *user* installs;
//!   the loader's answer for its absence is the same numeric code as for a
//!   missing runtime, so [`LoadError::MissingDependency`] is told apart from
//!   [`LoadError::NotFound`] by whether the file is on disk. That variant's own
//!   documentation says what was read out of the binary.
//!
//! ## The WebGPU plugin
//!
//! From 1.28 ONNX Runtime ships its WebGPU provider as a **plugin library**
//! registered at run time - `Microsoft.ML.OnnxRuntime.EP.WebGpu`, one
//! `onnxruntime_providers_webgpu` per platform - rather than compiling it into
//! every general release. The macOS archive still has it built in; the
//! Windows and Linux ones do not, and the plugin is what gives them the one
//! cross-vendor GPU provider ([`crate::accel`]). [`package`]
//! puts it beside the runtime on those platforms, and [`load`] registers it
//! when it is there: [`WEBGPU_PLUGIN_NAME`] in the runtime's own directory,
//! registered once, and reported through [`webgpu_plugin`] rather than
//! failing the load - a runtime whose plugin will not register is still a
//! runtime with a CPU in it.

pub mod package;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

/// The file name the runtime must keep. On macOS the dylib's own
/// `LC_LOAD_DYLIB` names `@rpath/libonnxruntime.1.dylib`, so a copy saved under
/// any other name cannot resolve its self-reference - and fails in a way that
/// reads exactly like a signing failure.
pub const DYLIB_NAME: &str = if cfg!(target_os = "windows") {
    "onnxruntime.dll"
} else if cfg!(target_os = "macos") {
    "libonnxruntime.dylib"
} else {
    "libonnxruntime.so"
};

/// The WebGPU plugin provider's library, as the package unpacks it beside
/// [`DYLIB_NAME`]. Present on Windows and Linux x64, where it is downloaded;
/// absent on macOS, where the provider is built into the runtime.
pub const WEBGPU_PLUGIN_NAME: &str = if cfg!(target_os = "windows") {
    "onnxruntime_providers_webgpu.dll"
} else if cfg!(target_os = "macos") {
    "libonnxruntime_providers_webgpu.dylib"
} else {
    "libonnxruntime_providers_webgpu.so"
};

/// The name the plugin is registered under. ONNX Runtime wants one per
/// library and does not read it; the provider's own name is what
/// [`crate::accel`] selects on.
const WEBGPU_PLUGIN_REGISTRATION: &str = "webgpu";

/// The ONNX Runtime release the application ships against. `ort` accepts
/// anything from its compile-time floor (the `api-NN` feature) upwards, so a
/// user with a newer runtime already installed is not forced to re-download.
pub const PINNED_VERSION: &str = "1.28.0";

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("no ONNX Runtime found; looked in: {}", .searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    NotFound { searched: Vec<PathBuf> },

    /// The file is there and the operating system refused to map it because it
    /// arrived quarantined. Recoverable without a re-download: clear the
    /// attribute. Kept distinct because the remedy is distinct.
    #[error("the ONNX Runtime at {path} is quarantined and cannot be loaded")]
    Quarantined { path: PathBuf },

    /// The file is there and was refused for a signing reason that clearing
    /// quarantine will not fix - on a hardened-runtime build, most likely the
    /// missing `disable-library-validation` entitlement.
    #[error("the ONNX Runtime at {path} was refused by the system: {detail}")]
    Refused { path: PathBuf, detail: String },

    /// The file is there, and the loader refused it because a library **it**
    /// imports is not on the machine. The remedy is an install the user
    /// performs, and it is neither a re-download nor a re-sign.
    ///
    /// On Windows this is the Visual C++ runtime. Parsing the PE import
    /// directory of the shipped
    /// `runtimes/packages/.dml/runtimes/win-x64/native/onnxruntime.dll`, its
    /// regular imports are `MSVCP140.dll`, `MSVCP140_1.dll`,
    /// `VCRUNTIME140.dll` and `VCRUNTIME140_1.dll` alongside `KERNEL32`,
    /// `ADVAPI32`, `SETUPAPI`, `dbghelp` and ten `api-ms-win-crt-*` names.
    /// Regular, not delay-loaded: they are resolved when the library is mapped,
    /// so their absence fails the load outright rather than the first call. The
    /// four `140` names are the Visual C++ 2015-2022 x64 Redistributable; the
    /// `api-ms-win-crt-*` ones beside them are the universal CRT and are part
    /// of Windows 10 and later, which is why only the first four are a user's
    /// problem. Nothing in the NuGet packages [`package`] downloads carries
    /// them, and nothing here downloads them separately.
    ///
    /// Kept out of [`LoadError::Failed`] because a generic "could not be
    /// loaded" sends a user to re-download a runtime that is already correct,
    /// and out of [`LoadError::NotFound`] because the runtime is on disk. The
    /// interface names it `diagnostics.runtime.missingDependency`.
    #[error("the ONNX Runtime at {path} is missing a library it depends on: {detail}")]
    MissingDependency { path: PathBuf, detail: String },

    #[error("the ONNX Runtime at {path} could not be loaded: {detail}")]
    Failed { path: PathBuf, detail: String },
}

/// Where to look, in order. `app_data` is the per-user directory a first-launch
/// download writes to; the caller supplies it because only the shell knows it.
///
/// `ORT_DYLIB_PATH` comes first so a developer can point at a build without
/// touching the installed copy - the same variable `ort` itself honours.
pub fn search_paths(app_data: Option<&Path>) -> Vec<PathBuf> {
    let explicit = std::env::var("ORT_DYLIB_PATH").ok();
    search_paths_from(explicit.as_deref(), app_data)
}

/// The same, with the override passed in rather than read.
///
/// Split out so the order can be tested without mutating the process
/// environment: two tests doing that concurrently is a race, and it produced
/// one - `cargo test` is parallel by default and `set_var` is global.
pub fn search_paths_from(explicit: Option<&str>, app_data: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(explicit) = explicit {
        if !explicit.is_empty() {
            paths.push(PathBuf::from(explicit));
        }
    }

    if let Some(dir) = app_data {
        paths.push(dir.join("runtimes").join(DYLIB_NAME));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Beside the executable, and - on macOS - the bundle's Resources,
            // which is `Contents/MacOS/../Resources`.
            paths.push(dir.join("runtimes").join(DYLIB_NAME));
            paths.push(dir.join("../Resources/runtimes").join(DYLIB_NAME));
        }
    }

    paths
}

/// The first search path that exists.
pub fn find(app_data: Option<&Path>) -> Result<PathBuf, LoadError> {
    let searched = search_paths(app_data);
    searched
        .iter()
        .find(|p| p.exists())
        .cloned()
        .ok_or(LoadError::NotFound { searched })
}

/// Load the runtime and commit the `ort` environment. Idempotent: `ort` holds
/// the library in a `OnceLock`, so later calls return the already-loaded one.
///
/// The WebGPU plugin beside it, where there is one, is registered on the
/// first successful load and never again - see [`webgpu_plugin`] for what
/// became of it.
pub fn load(path: &Path) -> Result<(), LoadError> {
    if let Some(dir) = path.parent() {
        add_library_directory(dir);
    }
    ort::init_from(path)
        .map_err(|e| {
            // Neither crate will hand over what the loader actually said.
            // `libloading` 0.9.0 does carry it - a `dlerror` string, or a
            // Windows code in `Error::LoadLibraryExW { source: WindowsError }` -
            // but `WindowsError`'s field is `pub(crate)`
            // (`libloading-0.9.0/src/error.rs:29`), so the number is reachable
            // only through that type's `Display`; and `ort`'s own `LoadError`
            // has an empty `impl core::error::Error` (`ort/src/lib.rs:118`), so
            // its `source()` is `None` and its `Display` prints
            // `libloading::Error`'s, which is the bare "dlopen failed" or
            // "LoadLibraryExW failed" with the source dropped
            // (`libloading-0.9.0/src/error.rs:136` and `:142`). So the loader is
            // asked again, which is also the only way to get the text that
            // separates a quarantined runtime from an unsigned one.
            let report = loader_report(path);
            let detail = report.detail.unwrap_or_else(|| e.to_string());
            classify(path, &detail, report.os_error)
        })?
        .with_name("manga-cleaner")
        .commit();
    let first = LOADED.get().is_none();
    LOADED.get_or_init(|| Loaded { path: path.to_path_buf(), build: build_of(path), file: file_stamp(path) });
    // The CUDA major wanted is read off the first load, so a report taken
    // before it is about no build at all. Later loads are the same library, and
    // are asked for on every Settings refresh and run: they must not cost the
    // GPU another test run beside a job.
    if first {
        crate::cuda::rescan();
    }
    WEBGPU_PLUGIN.get_or_init(|| register_webgpu_plugin(path));
    Ok(())
}

/// What the first successful [`load`] mapped, as it was at that moment.
struct Loaded {
    path: PathBuf,
    build: Option<Build>,
    file: Option<(u64, SystemTime)>,
}

static LOADED: OnceLock<Loaded> = OnceLock::new();

/// The record the app writes beside a runtime it unpacked: which build and
/// version that is. A dot-file, because `<app_data>/runtimes` is a directory a
/// user is invited to fill by hand.
pub const INSTALLED_FILE: &str = ".installed.json";

/// A runtime build, as its install record names it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Build {
    /// `stock`, `directml`, `cuda12` or `cuda13`.
    pub flavour: String,
    pub version: String,
}

/// The build the record beside `library` names, or `None` for **unknown**: a
/// runtime placed by hand, or one unpacked before the record existed. The
/// shipped packages all unpack to the same file name, so the path alone never
/// says which build it is.
pub fn build_of(library: &Path) -> Option<Build> {
    let record = library.parent()?.join(INSTALLED_FILE);
    serde_json::from_slice(&std::fs::read(record).ok()?).ok()
}

fn file_stamp(library: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(library).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// The library the first successful [`load`] mapped. A process loads one
/// runtime for its whole life, so this never changes once set. `None` before
/// any load.
pub fn loaded() -> Option<&'static Path> {
    LOADED.get().map(|loaded| loaded.path.as_path())
}

/// The build the loaded library is, read when it was loaded. [`crate::cuda`]
/// reads it to learn which CUDA major the build was compiled against, which no
/// API of the runtime itself reports.
pub fn loaded_build() -> Option<&'static Build> {
    LOADED.get()?.build.as_ref()
}

/// What tells this loaded library apart from another at the same path: its
/// build and version where the record names them, and its size and time
/// written either way, so a hand-placed library swapped for another is a
/// different one too.
pub fn loaded_identity() -> Option<String> {
    let loaded = LOADED.get()?;
    let build = loaded.build.as_ref().map_or_else(|| "build unknown".to_owned(), |b| format!("build {} {}", b.flavour, b.version));
    let file = loaded.file.map_or_else(String::new, |(len, time)| {
        let secs = time.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        format!(" file {len} {secs}")
    });
    Some(format!("{build}{file}"))
}

/// Whether the file at the loaded path is no longer the library this process
/// mapped: a download of another build replaced it. Until a restart the
/// process still runs the old one, so anything timed now is about a build the
/// next launch will not have.
pub fn replaced_since_load() -> bool {
    LOADED.get().is_some_and(|loaded| file_stamp(&loaded.path) != loaded.file || build_of(&loaded.path) != loaded.build)
}

/// What became of the WebGPU plugin beside the loaded runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plugin {
    /// No plugin library beside the runtime. macOS, whose archive has WebGPU
    /// built in, and Linux aarch64, for which none is published.
    Absent,
    /// The runtime already carries a `WebGpuExecutionProvider`, so the library
    /// was left unregistered: two providers of one name is a collision, and
    /// the built-in one is the measured one.
    BuiltIn,
    /// Registered; [`crate::accel`] can select it.
    Registered,
    /// Present and refused, with ONNX Runtime's reason - an ABI older than
    /// the plugin wants, or a library that would not open.
    Failed(String),
}

static WEBGPU_PLUGIN: OnceLock<Plugin> = OnceLock::new();

/// The plugin's state after the first [`load`]. [`Plugin::Absent`] before any
/// load, which is also what a machine with no plugin reads.
pub fn webgpu_plugin() -> Plugin {
    WEBGPU_PLUGIN.get().cloned().unwrap_or(Plugin::Absent)
}

fn register_webgpu_plugin(runtime: &Path) -> Plugin {
    use ort::ep::ExecutionProvider;

    let Some(dir) = runtime.parent() else { return Plugin::Absent };
    let plugin = dir.join(WEBGPU_PLUGIN_NAME);
    if !plugin.exists() {
        return Plugin::Absent;
    }
    if ort::ep::webgpu::WebGPU::default().is_available().unwrap_or(false) {
        return Plugin::BuiltIn;
    }
    let registered = ort::environment::Environment::current()
        .and_then(|env| env.register_ep_library(WEBGPU_PLUGIN_REGISTRATION, &plugin));
    match registered {
        // The handle only exists to unregister with, and nothing ever does:
        // the plugin lives as long as the process, like the runtime it is in.
        Ok(_handle) => Plugin::Registered,
        Err(e) => Plugin::Failed(e.to_string()),
    }
}

/// Put the runtime's directory on the loader's search path, and pin the one
/// library that has to be found through it.
///
/// Windows only, and for the libraries that are resolved **by name** rather
/// than the one this module opens by path. An earlier version of this comment
/// said `libloading` finds `DirectML.dll` beside `onnxruntime.dll` because it
/// opens the runtime with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR`. That is not the
/// mechanism, and neither half of it survives reading:
///
/// - **No search flag is passed.** `ort` 2.0.0-rc.13 opens the runtime with
///   `libloading::Library::new` (`ort/src/lib.rs:136`), and libloading 0.9.0's
///   Windows `new` is `load_with_flags(filename, 0)`
///   (`libloading-0.9.0/src/os/windows/mod.rs:69-70`). Zero: none of the
///   `LOAD_LIBRARY_SEARCH_*` flags is set, so the open adds the runtime's own
///   directory to nothing.
/// - **`DirectML.dll` is not resolved at load anyway.** Parsing the shipped
///   `onnxruntime.dll`'s PE data directories, `DirectML.dll`, `d3d12.dll` and
///   `dxgi.dll` are the *entire* delay-import table and appear nowhere in the
///   regular one. A delay-loaded import is resolved by the delay-load helper on
///   the first call into it, through an ordinary `LoadLibrary` against the
///   process search path - long after the flags of the open that mapped the
///   runtime have stopped mattering.
///
/// So what resolves DirectML is this function and nothing else.
/// `SetDllDirectoryW` inserts one directory into the search order of every
/// later `LoadLibrary` in the process: DirectML's, and the WebGPU plugin's
/// `dxcompiler.dll` and `dxil.dll`, which are opened by the plugin rather than
/// by anything that knows where the plugin came from. **It is load-bearing for
/// the DirectML flavour and not only for the plugin.** Deleted as plugin-only
/// cleanup, the DirectML build loses its provider on the first inference rather
/// than at load, where nothing would connect the two.
///
/// `SetDllDirectoryW` holds exactly **one** directory per process, and any
/// later caller anywhere in it silently replaces the entry. So the directory is
/// set *and* the library whose loss would be unrecoverable is opened
/// immediately, by absolute path, and never freed. A module already in the
/// process's loaded list is what `LoadLibrary` resolves a bare name to before
/// it searches any directory, so pinning `DirectML.dll` makes the delay-load
/// resolution independent of who owns the directory slot by then.
/// `ort::util::preload_dylib` is that call - it exists for this, and shipping
/// `DirectML.dll` beside the application is the example in its own
/// documentation. Where there is no `DirectML.dll` beside the runtime, which is
/// every non-DirectML flavour, nothing is pinned and nothing is reported: a
/// CUDA build without a DirectML is not a failure.
///
/// Unix loaders resolve a library's own dependencies through `ldconfig` and
/// `LD_LIBRARY_PATH`, which is where `libvulkan.so.1` lives, so nothing is
/// needed there.
#[cfg(windows)]
fn add_library_directory(dir: &Path) {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // A failure here is a directory that will be searched the ordinary way,
    // which is the state every earlier build was in; nothing to report.
    unsafe {
        windows_sys::Win32::System::LibraryLoader::SetDllDirectoryW(wide.as_ptr());
    }
    let directml = dir.join(DIRECTML_NAME);
    if directml.exists() {
        // Ignored deliberately. A pin that fails leaves the delay-load helper
        // to search for the name later exactly as it does today: this call
        // hardens that path, it does not replace it, and a runtime is still a
        // runtime without it.
        let _ = ort::util::preload_dylib(&directml);
    }
}

/// The DirectML library the Windows default flavour is unpacked beside, named
/// as `Microsoft.AI.DirectML` 1.15.4 spells it in `bin/x64-win` and as
/// `onnxruntime.dll`'s delay-import table spells it. See
/// [`add_library_directory`], which pins it, and [`package::Package`]'s
/// `companions` for how it gets there.
#[cfg(windows)]
const DIRECTML_NAME: &str = "DirectML.dll";

#[cfg(not(windows))]
fn add_library_directory(_dir: &Path) {}

/// Windows' `ERROR_MOD_NOT_FOUND`. 126, read off `windows-sys` 0.61.2's
/// `Win32::Foundation::ERROR_MOD_NOT_FOUND`; written out rather than imported
/// because this crate enables that crate's `Win32_System_LibraryLoader` and
/// `Win32_System_SystemInformation` features and not `Win32_Foundation`.
///
/// The loader returns it for **two** different things: a file it could not
/// find, and a file it found whose own imports it could not resolve. They are
/// told apart by the only thing that separates them, which is whether the file
/// is there - [`find`] has already answered [`LoadError::NotFound`] for a path
/// that is not on disk, and [`classify`] looks again rather than trusting that
/// nothing moved in between.
///
/// `ERROR_PROC_NOT_FOUND` (127) is deliberately not folded in. It is a
/// dependency that is present and too old, whose remedy is an upgrade rather
/// than an install, and nothing here has seen one; a case nobody has observed
/// is better reported as [`LoadError::Failed`] with the system's own sentence
/// on it than as a remedy this module guessed at.
const ERROR_MOD_NOT_FOUND: i32 = 126;

/// Attach a remedy to the loader's message.
///
/// `detail` is the platform's own sentence and `os_error` its numeric code
/// where it has one, both from [`loader_report`]. macOS is classified on the
/// text because the two refusals that matter there differ only in wording;
/// Windows is classified on the code, because the wording is localised and the
/// code is not.
fn classify(path: &Path, detail: &str, os_error: Option<i32>) -> LoadError {
    let detail = detail.to_owned();
    if detail.contains("disallowed by system policy") {
        LoadError::Quarantined { path: path.to_path_buf() }
    } else if detail.contains("Team ID") || detail.contains("code signature") {
        LoadError::Refused { path: path.to_path_buf(), detail }
    } else if os_error == Some(ERROR_MOD_NOT_FOUND) && path.exists() {
        LoadError::MissingDependency { path: path.to_path_buf(), detail }
    } else {
        LoadError::Failed { path: path.to_path_buf(), detail }
    }
}

/// What the loader said, asked a second time.
///
/// The load has already failed by the time one of these is built, so opening
/// the same file again re-runs the same refusal and reads the answer out of the
/// platform rather than out of an error type that dropped it - see [`load`] for
/// which types drop what.
#[derive(Default)]
struct LoaderReport {
    /// The operating system's own sentence about the failure.
    detail: Option<String>,
    /// The platform's numeric code, where the platform has one. Windows does.
    /// `dlerror` does not, and `errno` after a failed `dlopen` is not defined
    /// to carry the reason, so the Unix side leaves this `None` rather than
    /// reporting a number that means nothing.
    os_error: Option<i32>,
}

#[cfg(unix)]
fn loader_report(path: &Path) -> LoaderReport {
    use std::ffi::{CStr, CString};

    const RTLD_LAZY: std::os::raw::c_int = 0x1;
    const RTLD_LOCAL: std::os::raw::c_int = 0x4;
    unsafe extern "C" {
        fn dlopen(
            filename: *const std::os::raw::c_char,
            flag: std::os::raw::c_int,
        ) -> *mut std::os::raw::c_void;
        fn dlerror() -> *const std::os::raw::c_char;
    }

    let Ok(c_path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        return LoaderReport::default();
    };
    let detail = unsafe {
        dlerror();
        if !dlopen(c_path.as_ptr(), RTLD_LAZY | RTLD_LOCAL).is_null() {
            return LoaderReport::default();
        }
        let err = dlerror();
        if err.is_null() {
            None
        } else {
            Some(CStr::from_ptr(err).to_string_lossy().into_owned())
        }
    };
    LoaderReport { detail, os_error: None }
}

#[cfg(windows)]
fn loader_report(path: &Path) -> LoaderReport {
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // Flags zero and no `hFile`, which is what `libloading::Library::new`
    // passes (`libloading-0.9.0/src/os/windows/mod.rs:69-70`), so this repeats
    // the search `ort` just ran rather than a more forgiving one.
    let module = unsafe {
        windows_sys::Win32::System::LibraryLoader::LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    if !module.is_null() {
        return LoaderReport::default();
    }
    // Read before anything else can run: the thread's last-error is whatever
    // the most recent call that sets it left behind.
    let error = std::io::Error::last_os_error();
    LoaderReport { detail: Some(error.to_string()), os_error: error.raw_os_error() }
}

#[cfg(not(any(unix, windows)))]
fn loader_report(_path: &Path) -> LoaderReport {
    LoaderReport::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_override_is_searched_first() {
        let paths = search_paths_from(
            Some("/nowhere/libonnxruntime.dylib"),
            Some(Path::new("/app-data")),
        );
        assert_eq!(paths[0], PathBuf::from("/nowhere/libonnxruntime.dylib"));
        assert_eq!(paths[1], PathBuf::from("/app-data/runtimes").join(DYLIB_NAME));

        // An empty override is not an override.
        let without = search_paths_from(Some(""), Some(Path::new("/app-data")));
        assert_eq!(without[0], PathBuf::from("/app-data/runtimes").join(DYLIB_NAME));
    }

    /// Through `search_paths_from` rather than `find`, for exactly the reason
    /// that function was split out: `find` reads `ORT_DYLIB_PATH`, and a
    /// developer who has exported it - which the run instructions tell them
    /// to - has a runtime that is *not* absent. The test was then asserting on
    /// the shell it happened to be run from, and failed in one.
    #[test]
    fn an_absent_runtime_names_everywhere_it_looked() {
        let searched = search_paths_from(None, Some(Path::new("/definitely/not/here")));
        let message = LoadError::NotFound { searched }.to_string();
        assert!(message.contains("/definitely/not/here"), "{message}");
    }

    /// The one test that touches a real runtime. It is skipped, loudly, when
    /// `scripts/fetch-runtime.sh` has not been run - a developer without the
    /// download should not see a red test, and a developer with one should not
    /// have the load go unexercised.
    #[test]
    fn an_installed_runtime_loads_and_reports_itself() {
        let dev_copy = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../runtimes/onnxruntime-osx-arm64-1.28.0/lib")
            .join(DYLIB_NAME);
        if !dev_copy.exists() {
            eprintln!("skipped: no runtime at {} - run scripts/fetch-runtime.sh", dev_copy.display());
            return;
        }
        load(&dev_copy).expect("the installed runtime loads");
        assert!(ort::info().contains("ORT Build Info"), "{}", ort::info());
    }

    #[test]
    fn the_quarantine_refusal_is_told_apart_from_a_signing_refusal() {
        let path = Path::new("/nonexistent/libonnxruntime.dylib");
        assert!(matches!(
            classify(path, "library load disallowed by system policy", None),
            LoadError::Quarantined { .. }
        ));
        assert!(matches!(
            classify(path, "have different Team IDs", None),
            LoadError::Refused { .. }
        ));
        assert!(matches!(classify(path, "no such file", None), LoadError::Failed { .. }));
    }

    /// Windows' 126 means two different things and the file on disk is what
    /// separates them: a runtime that is not there is [`LoadError::NotFound`]
    /// by way of [`find`], and one that is there and still answers 126 is a
    /// runtime whose own imports could not be resolved.
    ///
    /// Written against a file this test creates rather than against the shipped
    /// runtime, because the branch under test is `path.exists()` and nothing
    /// else - the code is supplied, not produced, since this repository has
    /// never executed on Windows and cannot make a real 126 happen.
    #[test]
    fn a_present_file_that_answers_mod_not_found_is_a_missing_dependency() {
        let absent = Path::new("/no/such/directory/onnxruntime.dll");
        assert!(
            matches!(
                classify(absent, "The specified module could not be found.", Some(ERROR_MOD_NOT_FOUND)),
                LoadError::Failed { .. }
            ),
            "a file that is not on disk was reported as a missing dependency"
        );

        let dir = std::env::temp_dir().join(format!("mc-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let present = dir.join("onnxruntime.dll");
        std::fs::write(&present, b"not a PE file").unwrap();
        assert!(matches!(
            classify(&present, "The specified module could not be found.", Some(ERROR_MOD_NOT_FOUND)),
            LoadError::MissingDependency { .. }
        ));

        // Any other code on the same present file stays generic: 127 is a
        // dependency that is there and too old, and this module does not claim
        // to know that remedy.
        assert!(matches!(
            classify(&present, "The specified procedure could not be found.", Some(127)),
            LoadError::Failed { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
