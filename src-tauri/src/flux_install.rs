//! Installs the optional local FLUX process and its pinned checkpoint on demand.
//! The shipped Python sources are app resources; provider credentials are not used.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{Emitter, Manager};

const INSTALL_LIMIT: Duration = Duration::from_secs(2 * 60 * 60);
static INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn source_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let bundled = app.path().resource_dir().map_err(|e| e.to_string())?.join("sidecar");
    if bundled.join("bootstrap.py").is_file() && bundled.join("pyproject.toml").is_file() {
        return Ok(bundled);
    }
    if cfg!(debug_assertions) {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../sidecar");
        if checkout.join("bootstrap.py").is_file() {
            return Ok(checkout);
        }
    }
    Err("This app build does not include the FLUX helper source".into())
}

fn system_python() -> Option<(String, Vec<String>)> {
    let mut candidates = vec![
        ("python3".to_string(), Vec::new()),
        ("python".to_string(), Vec::new()),
    ];
    if cfg!(windows) {
        for version in ["-3.12", "-3.11", "-3.10"] {
            candidates.push(("py".to_string(), vec![version.to_string()]));
        }
    }
    candidates.into_iter().find(|(name, prefix)| {
        Command::new(name)
            .args(prefix)
            .args(["-c", "import sys; assert (3,10) <= sys.version_info[:2] < (3,13)"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

pub(crate) fn managed_python(app: &tauri::AppHandle, root: &Path, version: &str) -> Result<(String, Vec<String>), String> {
    if !["3.11", "3.12"].contains(&version) {
        return Err("Unsupported managed Python version".into());
    }
    let name = if cfg!(windows) { "manga-cleaner-uv.exe" } else { "manga-cleaner-uv" };
    let bundled = std::env::current_exe().ok().and_then(|path| path.parent().map(|dir| dir.join(name)));
    let uv = bundled.filter(|path| path.is_file()).unwrap_or_else(|| PathBuf::from("uv"));
    let python_dir = root.join("python");
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let _ = app.emit("flux-install://progress", json!({"step": "environment", "state": "start"}));
    let mut child = Command::new(&uv)
        .args(["python", "install", version, "--install-dir"])
        .arg(&python_dir)
        .env("UV_PYTHON_INSTALL_DIR", &python_dir)
        .env("UV_PYTHON_BIN_DIR", root.join("python-bin"))
        .env("UV_NO_CONFIG", "1")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn()
        .map_err(|_| "This app build has no managed Python installer".to_string())?;
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() { return Err("Could not download the managed Python runtime".into()); }
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Downloading the Python runtime timed out".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let found = Command::new(&uv)
        .args(["python", "find", version, "--managed-python", "--system"])
        .env("UV_PYTHON_INSTALL_DIR", &python_dir)
        .env("UV_NO_CONFIG", "1")
        .stdin(Stdio::null()).output()
        .map_err(|e| format!("Could not locate the managed Python runtime: {e}"))?;
    if !found.status.success() { return Err("Managed Python was downloaded but could not be found".into()); }
    let path = String::from_utf8(found.stdout).map_err(|_| "Invalid managed Python path")?;
    let path = path.trim();
    if !Path::new(path).is_file() { return Err("Managed Python path is missing".into()); }
    Ok((path.to_string(), Vec::new()))
}

fn install(app: &tauri::AppHandle, backend: &str, accelerator: &str) -> Result<Value, String> {
    if !["auto", "mflux", "sdnq"].contains(&backend) {
        return Err("Choose Automatic, MLX, or SDNQ for the FLUX backend".into());
    }
    if !["auto", "cuda", "xpu", "mps"].contains(&accelerator) {
        return Err("Unsupported FLUX accelerator".into());
    }
    let _guard = INSTALL_LOCK.get_or_init(|| Mutex::new(())).lock().map_err(|e| e.to_string())?;
    let source = source_dir(app)?;
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?.join("sidecar");
    let (python, prefix) = match system_python() {
        Some(existing) => existing,
        None => managed_python(app, &root, "3.11")?,
    };
    let mut child = Command::new(python)
        .args(prefix)
        .arg(source.join("bootstrap.py"))
        .args(["--root", root.to_str().ok_or("Invalid FLUX install path")?])
        .args(["--source", source.to_str().ok_or("Invalid FLUX source path")?])
        .args(["--backend", backend, "--accelerator", accelerator])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start FLUX setup: {e}"))?;
    let output = child.stdout.take().ok_or("FLUX setup output is unavailable")?;
    let emitter = app.clone();
    let reader = std::thread::spawn(move || {
        let mut result = None;
        let mut error = None;
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if line.len() > 2048 {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
            if let (Some(step), Some(state)) = (value["step"].as_str(), value["state"].as_str()) {
                if ["environment", "dependencies", "weights", "ready", "error"].contains(&step)
                    && ["start", "done", "ValueError", "RuntimeError", "TimeoutExpired", "FileNotFoundError"].contains(&state)
                {
                    let _ = emitter.emit("flux-install://progress", json!({"step": step, "state": state}));
                }
            }
            if value["result"].is_object() { result = Some(value["result"].clone()); }
            if let Some(message) = value["error"].as_str() { error = Some(message.to_string()); }
        }
        (result, error)
    });
    let deadline = Instant::now() + INSTALL_LIMIT;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? { break status; }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("FLUX setup timed out; retry to continue the download".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let (result, error) = reader.join().map_err(|_| "FLUX setup output failed")?;
    if !status.success() {
        return Err(error.unwrap_or_else(|| "FLUX setup failed; retry to continue".into()));
    }
    result.ok_or_else(|| "FLUX setup finished without a result".into())
}

#[tauri::command]
pub async fn install_flux_helper(
    app: tauri::AppHandle,
    backend: String,
    accelerator: String,
) -> Result<Value, String> {
    crate::library::blocking(move || install(&app, &backend, &accelerator)).await
}
