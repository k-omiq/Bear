//! Background jobs: what is running, for the interface and for quitting.
//!
//! A Detect, Clean or cloud Clean run holds a slot in [`crate::run`]; a local or
//! cloud denoise holds an entry in [`crate::page_denoise`]'s registry. Either
//! goes on when the dialog that started it closes or the editor is left, so
//! [`list_jobs`] is how a reloaded webview or a reopened chapter finds them
//! again, and the quit guard is what keeps a quit from dropping them unasked.
//!
//! ## The quit guard
//!
//! Closing the main window with close-to-tray off, the tray's Quit, and an
//! exit asked for in code (macOS Cmd+Q included, see `lib.rs`) all ask
//! [`quit_decision`] first. With jobs running and the quit not yet confirmed it
//! is held, and `app://quit-requested` asks the interface, which answers with
//! [`confirm_quit`] (stop every job, then exit) or [`hide_to_tray`]. With none
//! running a quit goes exactly as it did before.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

/// The event that asks the interface whether to quit with jobs running.
pub const QUIT_REQUESTED_EVENT: &str = "app://quit-requested";

/// How long [`confirm_quit`] waits for the jobs it stopped to wind down. A
/// run checks its flag between regions and a local denoise between tiles, so
/// most stop well inside it; a cloud page in flight can take minutes, and is
/// not waited for past this.
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// One job in flight, as `list_jobs` answers it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub run_id: String,
    /// `detect`, `clean`, `cloudClean`, `denoise` or `cloudDenoise`.
    pub kind: &'static str,
    pub chapter_id: String,
    pub done: u32,
    pub total: u32,
}

/// Every job in flight: the runs in the order they started, then the denoise
/// runs.
pub fn snapshot() -> Vec<JobView> {
    let mut jobs = crate::run::jobs();
    jobs.extend(crate::page_denoise::jobs());
    jobs
}

/// How many jobs are in flight.
pub fn running() -> usize {
    snapshot().len()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuitRequested {
    jobs: usize,
    can_hide: bool,
}

/// Set once the user has said to stop the jobs and quit, so the exit that
/// follows is not held again.
static CONFIRMED: AtomicBool = AtomicBool::new(false);

/// What a quit does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitDecision {
    /// Let it go through.
    Proceed,
    /// Hold it and ask: this many jobs would be dropped.
    Ask { jobs: usize },
}

/// Whether a quit goes through: always once confirmed, always with nothing
/// running, and otherwise held to ask.
pub(crate) fn quit_decision(running: usize, confirmed: bool) -> QuitDecision {
    if confirmed || running == 0 { QuitDecision::Proceed } else { QuitDecision::Ask { jobs: running } }
}

/// The number of jobs a quit asked for now would drop, when it must be held.
pub(crate) fn quit_held() -> Option<usize> {
    match quit_decision(running(), CONFIRMED.load(Ordering::SeqCst)) {
        QuitDecision::Proceed => None,
        QuitDecision::Ask { jobs } => Some(jobs),
    }
}

/// Ask the interface whether to quit with `jobs` running.
pub(crate) fn ask_quit<R: tauri::Runtime>(app: &tauri::AppHandle<R>, jobs: usize, can_hide: bool) {
    use tauri::Emitter;
    let _ = app.emit(QUIT_REQUESTED_EVENT, QuitRequested { jobs, can_hide });
}

/// Stop every run and denoise run and wait, up to `wait`, for them to let go.
/// Stopped rather than dropped, so a run's journal records it interrupted and
/// the chapter offers a resume. Answers how many were still going at the end.
fn stop_all(wait: Duration) -> usize {
    crate::run::cancel_all();
    crate::page_denoise::cancel_all();
    let deadline = Instant::now() + wait;
    loop {
        let left = running();
        if left == 0 || Instant::now() >= deadline { return left; }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `listJobs`: every job in flight.
#[tauri::command]
pub fn list_jobs() -> Vec<JobView> {
    snapshot()
}

/// `confirmQuit`: stop every job, wait briefly for them to wind down, and exit.
#[tauri::command]
pub async fn confirm_quit(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        CONFIRMED.store(true, Ordering::SeqCst);
        let left = stop_all(QUIT_WAIT);
        if left > 0 {
            eprintln!("manga-cleaner: quitting with {left} job(s) still stopping");
        }
        app.exit(0);
    })
    .await
    .map_err(|e| e.to_string())
}

/// `hideToTray`: hide the main window and keep the jobs running. Answers false,
/// and hides nothing, when there is no tray icon to bring the window back.
#[tauri::command]
pub fn hide_to_tray(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri::Manager;
    if app.tray_by_id(crate::TRAY_ID).is_none() { return Ok(false); }
    let Some(window) = app.get_webview_window("main") else { return Ok(false) };
    window.hide().map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quit_is_held_only_with_jobs_running_and_unconfirmed() {
        assert_eq!(quit_decision(0, false), QuitDecision::Proceed);
        assert_eq!(quit_decision(0, true), QuitDecision::Proceed);
        assert_eq!(quit_decision(2, false), QuitDecision::Ask { jobs: 2 });
        assert_eq!(quit_decision(2, true), QuitDecision::Proceed, "a confirmed quit is not asked twice");
    }

    #[test]
    fn the_quit_question_is_camel_case() {
        let asked = serde_json::to_value(QuitRequested { jobs: 3, can_hide: true }).unwrap();
        assert_eq!(asked, serde_json::json!({ "jobs": 3, "canHide": true }));
    }
}
