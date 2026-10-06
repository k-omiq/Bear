//! A bounded human review before Qwen's cached result can be attached.
use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Emitter;
use super::service::{InferenceServiceError, QwenPreview, ReviewChoice, ReviewSink};

fn waiting() -> &'static Mutex<HashMap<String, mpsc::Sender<ReviewChoice>>> {
    static WAITING: OnceLock<Mutex<HashMap<String, mpsc::Sender<ReviewChoice>>>> = OnceLock::new();
    WAITING.get_or_init(Default::default)
}

pub fn review_sink(app: &tauri::AppHandle) -> ReviewSink {
    let app = app.clone();
    Arc::new(move |preview: &QwenPreview| {
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = waiting().lock().unwrap();
            if pending.contains_key(&preview.attempt_id) {
                return Err(InferenceServiceError::ReviewRequired);
            }
            pending.insert(preview.attempt_id.clone(), tx);
        }
        let result = (|| {
            app.emit("qwen://review", preview).map_err(|_| InferenceServiceError::ReviewRequired)?;
            let until = Instant::now() + Duration::from_secs(600);
            loop {
                if Instant::now() >= until || !super::cloud_allowed(&app) || super::service::render_cancelled(&preview.attempt_id) {
                    return Ok(ReviewChoice::Discard);
                }
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(choice) => return Ok(if choice == ReviewChoice::Retry && !preview.can_retry { ReviewChoice::Discard } else { choice }),
                    Err(mpsc::RecvTimeoutError::Timeout) => {},
                    Err(_) => return Ok(ReviewChoice::Discard),
                }
            }
        })();
        waiting().lock().unwrap().remove(&preview.attempt_id);
        // Cached recovery can wait for review without attempt-progress events.
        // Always retire its UI as well, including timeout, cancellation, and
        // cloud disable, so stale review controls cannot remain actionable.
        let _ = app.emit("qwen://review", serde_json::json!({
            "attemptId": preview.attempt_id,
            "closed": true,
        }));
        result
    })
}

#[tauri::command]
pub fn resolve_qwen_review(attempt_id: String, choice: ReviewChoice) -> Result<(), String> {
    let map = waiting().lock().unwrap();
    map.get(&attempt_id).ok_or("qwen_review_gone")?.send(choice).map_err(|_| "qwen_review_gone".to_string())
}
