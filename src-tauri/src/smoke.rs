//! The launch check: open the window, prove the page works, write a report,
//! exit.
//!
//! Start the app with `MANGA_CLEANER_SMOKE_REPORT=<file>` and it runs one
//! check instead of a session. Nothing else about the launch changes, so a
//! missing system library, a webview that will not start, or a page that
//! throws before it mounts fails here the way it would for a user, and the
//! report says which. Release CI runs it on every Windows and Linux build,
//! because no other test ever opens the window there.
//!
//! What it proves, in order:
//!
//! - the page mounted (`#app` has children) and stayed mounted;
//! - a command round trip works (`diagnostics`);
//! - the `tile` scheme answers a cross-origin `fetch` from the page, which is
//!   how every layer and mask image reaches the editor;
//! - with `MANGA_CLEANER_SMOKE_RUNTIME=1`, the ONNX Runtime for this platform
//!   downloads, verifies and loads, and which accelerators it offers.
//!
//! The page half is `smoke.js`. It reports back over the event channel the
//! window already has (`core:default`), so the check adds no command to the
//! window's capability.
//!
//! Exit code: 0 when every check passed, 1 when one failed, 3 when the page
//! never reported. The window stays up for [`HOLD`] after the report is
//! written, so CI can take a screenshot of it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tauri::{AppHandle, Listener, Manager};

pub const REPORT_ENV: &str = "MANGA_CLEANER_SMOKE_REPORT";
pub const RUNTIME_ENV: &str = "MANGA_CLEANER_SMOKE_RUNTIME";

/// The event `smoke.js` sends its findings on.
const FRONTEND_EVENT: &str = "smoke-frontend";

/// How long the page has to load and report.
const FRONTEND_DEADLINE: Duration = Duration::from_secs(120);

/// How long the runtime has to download and unpack: up to about 455 MB on a
/// CI runner.
const RUNTIME_DEADLINE: Duration = Duration::from_secs(900);

/// How long the window stays up after the report is written.
const HOLD: Duration = Duration::from_secs(5);

const SCRIPT: &str = include_str!("smoke.js");

static FINISHED: AtomicBool = AtomicBool::new(false);
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Where the report goes, when this launch is a check.
pub fn report_path() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var_os(REPORT_ENV).filter(|value| !value.is_empty()).map(PathBuf::from))
        .as_deref()
}

pub fn requested() -> bool {
    report_path().is_some()
}

fn wants_runtime() -> bool {
    std::env::var(RUNTIME_ENV).is_ok_and(|value| value == "1")
}

/// Start the deadline and listen for the page's report. Called from `setup`.
pub fn begin(app: &AppHandle) {
    let deadline = FRONTEND_DEADLINE + if wants_runtime() { RUNTIME_DEADLINE } else { Duration::ZERO };
    let watchdog = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(deadline);
        let report = base(&watchdog, "timeout", json!({ "reported": REPORTED.load(Ordering::SeqCst) }));
        finish(report, 3);
    });
    let handle = app.clone();
    app.listen_any(FRONTEND_EVENT, move |event| {
        if REPORTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let frontend: Value = serde_json::from_str(event.payload()).unwrap_or(Value::Null);
        let app = handle.clone();
        std::thread::spawn(move || complete(&app, frontend));
    });
}

/// Run the page half once the main window's page has loaded.
pub fn on_page_load(webview: &tauri::Webview, payload: &tauri::webview::PageLoadPayload<'_>) {
    let finished = matches!(payload.event(), tauri::webview::PageLoadEvent::Finished);
    if requested() && finished && webview.label() == "main" {
        if let Err(error) = webview.eval(SCRIPT) {
            finish(base(webview.app_handle(), "frontend", json!({ "evalError": error.to_string() })), 1);
        }
    }
}

fn complete(app: &AppHandle, frontend: Value) {
    let frontend_ok = frontend.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let mut report = base(app, "frontend", frontend);
    let mut ok = frontend_ok;
    if frontend_ok && wants_runtime() {
        let (runtime_ok, runtime) = runtime(app);
        report["stage"] = json!("runtime");
        report["runtime"] = runtime;
        ok = runtime_ok;
    }
    report["ok"] = json!(ok);
    finish(report, if ok { 0 } else { 1 });
}

/// Download the runtime the way the first-launch dialog does, then load it.
fn runtime(app: &AppHandle) -> (bool, Value) {
    use crate::events::{self, Event};
    use crate::weights::{self, DownloadStart, RUNTIME_ID};

    let (sender, receiver) = std::sync::mpsc::channel();
    let sink = events::register(Box::new(move |event| {
        if let Event::ModelProgress { id, done: true, error, .. } = event {
            if id == RUNTIME_ID {
                let _ = sender.send(error.clone());
            }
        }
    }));
    let download = match weights::download_runtime(app.clone()) {
        Ok(DownloadStart::AlreadyInstalled) => Ok(()),
        Ok(DownloadStart::Started | DownloadStart::AlreadyRunning) => match receiver.recv_timeout(RUNTIME_DEADLINE) {
            Ok(None) => Ok(()),
            Ok(Some(error)) => Err(error),
            Err(_) => Err("the download did not finish in time".to_owned()),
        },
        Err(error) => Err(error),
    };
    events::unregister(sink);
    let status = crate::diagnostics::onnx_runtime(app);
    let ok = download.is_ok() && status.available;
    let accelerators = crate::models::list_accelerators(app.clone());
    (ok, json!({ "downloadError": download.err(), "onnxRuntime": status, "accelerators": accelerators }))
}

fn base(app: &AppHandle, stage: &str, frontend: Value) -> Value {
    json!({
        "ok": false,
        "stage": stage,
        "appVersion": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "webview": tauri::webview_version().unwrap_or_else(|error| format!("unavailable: {error}")),
        "appDataDir": app.path().app_data_dir().ok(),
        "frontend": frontend,
    })
}

/// Write the report once, hold the window for a screenshot, and end the
/// process. `std::process::exit` rather than `AppHandle::exit`: the deadline
/// fires exactly when the event loop may be stuck.
fn finish(report: Value, code: i32) {
    if FINISHED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(path) = report_path() else { return };
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|error| format!("{{\"serializeError\":\"{error}\"}}"));
    // Whole or absent: CI reads the file as soon as it appears.
    let staged = path.with_extension("partial");
    if let Err(error) = std::fs::write(&staged, &text).and_then(|()| std::fs::rename(&staged, path)) {
        eprintln!("smoke: could not write {}: {error}", path.display());
    }
    eprintln!("smoke: exit {code}\n{text}");
    std::thread::sleep(HOLD);
    std::process::exit(code);
}
