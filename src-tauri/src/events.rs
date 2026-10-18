//! The event channel: the five events, and the registry that
//! carries them to the webview.
//!
//! The seam has one rule about progress and it is absolute: **progress arrives
//! only on the event channel**. `subscribe(handler)` is one handler and many
//! events, never a callback per call, because a Tauri `invoke` cannot return a
//! streaming result and no component may come to depend on timing a real
//! backend could not provide.
//!
//! ## Why a registry rather than `AppHandle::emit`
//!
//! Tauri's own event system would work and is the wrong shape. `emit` is a
//! broadcast keyed by a string name, delivered to whatever listeners a window
//! happens to have; the seam wants a single ordered stream owned by one
//! subscriber. A [`Channel`](tauri::ipc::Channel) is exactly that - the
//! frontend constructs it, hands it to a command, and every message the
//! backend sends on it arrives at that one handler, in order. So `subscribe`
//! becomes: build a channel, register it here, and unregister it when the last
//! handler goes.
//!
//! The registry is a process-global rather than Tauri managed state for one
//! reason: `Builder::manage` is called in `lib.rs`, which the run is not
//! allowed to edit, and a `manage` call made later from inside a command
//! races the first caller. A `OnceLock<Mutex<…>>` needs neither.
//!
//! ## Why a sink and not a channel
//!
//! [`Sink`] is a closure rather than a `Channel<Event>` so that the run
//! scheduler - where all the ordering
//! rules live - can be tested against a collector without a Tauri runtime.
//! What the tests exercise is then the same code path the window uses, with a
//! different last inch.
//!
//! ## What this closes
//!
//! Until now the ten implemented
//! seam methods emitted nothing, so inside a Tauri window every notice they
//! owed was silently gone - `notice.project.created`, `notice.chapter.added`,
//! `notice.export.finished`, and the input reports persisted so that
//! `openChapter` could stagger them. They are emitted from the command
//! wrappers rather than from [`crate::library::Library`]'s methods, because
//! the model is deliberately testable without a runtime and a global emit
//! inside it would leak between unit tests.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::library::{ApiPage, ApiRegion};

/// The five events, in their fixed shapes.
///
/// `#[serde(tag = "type")]` puts the discriminator in the payload under the
/// name the seam uses, so what reaches the handler is `{type: 'page-started',
/// runId, …}` with no mapping layer on the JavaScript side - the same choice
/// [`crate::exporting::ExportResult`] makes.
///
/// `page-started` is a variant here, not a flag on `region-done`, because it is
/// ruled a **fifth event type**:
/// a page's `●` mark comes from it, and there is otherwise no way to say "this
/// page is being cleaned now".
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Event {
    #[serde(rename_all = "camelCase")]
    PageStarted {
        run_id: String,
        chapter_id: String,
        page_id: String,
        page_index: u32,
    },
    #[serde(rename_all = "camelCase")]
    RegionDone {
        run_id: String,
        chapter_id: String,
        page_id: String,
        page_index: u32,
        region: Box<ApiRegion>,
    },
    #[serde(rename_all = "camelCase")]
    PageDone {
        run_id: String,
        chapter_id: String,
        page_id: String,
        page_index: u32,
        page: Box<ApiPage>,
    },
    /// Exactly one per run, whatever happened. `next_page_index` is set **only**
    /// on cancel, and is where a resume picks up.
    #[serde(rename_all = "camelCase")]
    RunFinished {
        run_id: String,
        chapter_id: String,
        reason: &'static str,
        pages_queued: u32,
        pages_cleaned: u32,
        regions_cleaned: u32,
        next_page_index: Option<u32>,
    },
    /// A model or the ONNX Runtime is being fetched.
    ///
    /// A **sixth** event type rather than a notice, and rather than a resolved
    /// promise on `downloadModel`, for the rule the module docs above state
    /// absolutely: progress arrives only on the event channel. A 207 MB
    /// download reports for a minute or more, has to be cancellable while it
    /// does, and a Tauri `invoke` can return none of that.
    ///
    /// `id` is a [`crate::weights::MODELS`] row's id, or
    /// [`crate::weights::RUNTIME_ID`]. `total` is the response's
    /// `Content-Length` and is absent for a server that does not send one.
    /// **Exactly one event per download carries `done`**, whatever happened:
    /// `error` is `None` for a file that arrived and verified, `Some` for a
    /// failure, a digest mismatch, or a cancellation - the interface takes the
    /// row back to "not installed" through the same path for all three.
    #[serde(rename_all = "camelCase")]
    ModelProgress {
        id: String,
        downloaded: u64,
        total: Option<u64>,
        done: bool,
        error: Option<String>,
    },
    /// The backend recorded an undo step of its own on this chapter: a
    /// result committed, a detected region was cleaned, a result was
    /// rejected (`history::record_step`). The interface reads the chapter's
    /// index again if that chapter is the open one. No payload: the index is
    /// read under the same lock every other history read takes.
    #[serde(rename_all = "camelCase")]
    HistoryChanged {
        chapter_id: String,
    },
    /// The transient stack. `key` and `params` are i18n data; no English
    /// crosses the seam.
    Notice {
        id: String,
        key: String,
        params: serde_json::Value,
        tone: &'static str,
    },
}

/// One subscriber. See the module docs for why this is a closure.
pub type Sink = Box<dyn Fn(&Event) + Send + Sync>;

struct Registry {
    sinks: Vec<(u64, Sink)>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry { sinks: Vec::new() }))
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The registry, poison and all.
///
/// **A poisoned registry is recovered, never abandoned.** The mutex is held
/// while the sinks run, so one sink that panics poisons it - and treating that
/// as "give up" would end the event stream for the life of the *process*: every
/// later `emit` would return silently, and the first run after it would show a
/// page that never finishes and offer a cancel that never answers. The panic
/// has already been reported by the thread it happened on; what is left is a
/// registry whose contents are a `Vec` of closures, which no half-finished
/// write can have corrupted. So the lock is taken through the poison.
fn locked() -> std::sync::MutexGuard<'static, Registry> {
    registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Add a sink, and answer with the handle that removes it again.
pub fn register(sink: Sink) -> u64 {
    let id = next_id();
    locked().sinks.push((id, sink));
    id
}

pub fn unregister(id: u64) -> bool {
    let mut registry = locked();
    let before = registry.sinks.len();
    registry.sinks.retain(|(existing, _)| *existing != id);
    registry.sinks.len() != before
}

/// Send one event to every subscriber.
///
/// A sink that panics takes the emitting thread down with it - that is the
/// caller's panic, not something to swallow here - but it does not take the
/// channel: the next `emit` recovers the poisoned registry and delivers. See
/// [`locked`].
pub fn emit(event: &Event) {
    let registry = locked();
    for (_, sink) in &registry.sinks {
        sink(event);
    }
}

/// A notice, with an id the stack can key on.
pub fn notice(key: &str, params: serde_json::Value, tone: &'static str) {
    emit(&notice_event(key, params, tone));
}

/// The notice event itself, for a caller that has its own sink.
///
/// The run's `report` needs one: it is emitted through the sink the scheduler
/// was handed rather than through the global registry, so that the notice a run
/// ends on is testable beside the events it ends with.
///
/// **The id is prefixed, and the prefix is the point.** The counter is
/// process-wide and monotonic, which is what the notice stack keys on - but
/// monotonic *here* is not unique *there*. `subscribe` is a merge of this
/// stream with the mock's, the mock
/// mints `notice-1`, `notice-2`, … of its own, and the two counters know
/// nothing about each other: unprefixed, `notice-7` would name one notice from
/// each and the stack would key them as one. The same reasoning gives the run
/// its own prefix in [`crate::run`].
pub fn notice_event(key: &str, params: serde_json::Value, tone: &'static str) -> Event {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    Event::Notice {
        id: format!("backend-notice-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        key: key.to_owned(),
        params,
        tone,
    }
}

/// Emit a run of notices with a gap between them, off the calling thread.
///
/// `openChapter` owes several at once - junk skipped, a duplicate basename, a
/// file refused - and the notice stack is a queue a user reads. The mock
/// staggers them by `noticeStagger` for that reason and the interface is
/// written against that behaviour; landing four at the same millisecond makes
/// the first three unreadable.
pub fn stagger(notices: Vec<(String, serde_json::Value, &'static str)>) {
    if notices.is_empty() {
        return;
    }
    // `spawn_blocking` rather than `spawn`: this sleeps, and a sleeping tokio
    // worker is one fewer worker for everything else on the runtime - the
    // reason `library::blocking` exists at all.
    tauri::async_runtime::spawn_blocking(move || {
        for (key, params, tone) in notices {
            std::thread::sleep(STAGGER);
            notice(&key, params, tone);
        }
    });
}

/// The mock's `noticeStagger`, which is what the interface was built against.
const STAGGER: std::time::Duration = std::time::Duration::from_millis(260);

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

/// Register the webview's channel. The backend half of `subscribe`.
///
/// Answers with the handle [`unsubscribe_events`] takes. The frontend opens one
/// channel per window and holds it (`src/lib/api/tauri-events.js`): a run thread
/// emits for minutes with no call outstanding, so a sink whose lifetime is a
/// handler's drops whatever falls between two navigations. The handle stays for a caller with
/// a shorter lifetime than the window; there is none today.
///
/// `async` for the reason every command in this crate is - see
/// [`crate::library::blocking`] - though the body itself is a lock and a push.
#[tauri::command]
pub async fn subscribe_events(channel: tauri::ipc::Channel<Event>) -> Result<u64, String> {
    Ok(register(Box::new(move |event| {
        // A send failure means the window has gone. There is nothing useful to
        // do about it here and nothing to report it to; the stale sink is
        // dropped by `unsubscribe_events`, or with the process.
        let _ = channel.send(event.clone());
    })))
}

#[tauri::command]
pub async fn unsubscribe_events(id: u64) -> Result<bool, String> {
    Ok(unregister(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The registry is process-wide, and `cargo test` is parallel by default.
    /// Two tests emitting at once would each see the other's events, which is
    /// not a property of the registry - it is a property of sharing one. So
    /// the tests that emit take this first.
    fn exclusively() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A collector, and the guard that takes it out of the registry again.
    /// Every test that touches the global registry uses this, so a leaked sink
    /// cannot make a later test see another test's events.
    struct Collected {
        id: u64,
        seen: Arc<Mutex<Vec<Event>>>,
    }

    impl Collected {
        fn new() -> Collected {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&seen);
            let id = register(Box::new(move |event| sink.lock().unwrap().push(event.clone())));
            Collected { id, seen }
        }

        fn keys(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .filter_map(|event| match event {
                    Event::Notice { key, .. } => Some(key.clone()),
                    _ => None,
                })
                .collect()
        }
    }

    impl Drop for Collected {
        fn drop(&mut self) {
            unregister(self.id);
        }
    }

    #[test]
    fn a_registered_sink_receives_events_and_a_dropped_one_does_not() {
        let _guard = exclusively();
        let collected = Collected::new();
        notice("notice.run.cancelled", serde_json::json!({}), "warn");
        assert_eq!(collected.keys(), vec!["notice.run.cancelled".to_string()]);

        let id = collected.id;
        assert!(unregister(id));
        notice("notice.run.finished", serde_json::json!({}), "info");
        assert_eq!(collected.keys(), vec!["notice.run.cancelled".to_string()]);
        assert!(!unregister(id), "unregistering twice reported a second removal");
    }

    #[test]
    fn every_notice_has_an_id_of_its_own() {
        let _guard = exclusively();
        let collected = Collected::new();
        notice("notice.run.finished", serde_json::json!({}), "info");
        notice("notice.run.finished", serde_json::json!({}), "info");
        let ids: Vec<String> = collected
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                Event::Notice { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1], "two notices shared a stack key");
    }

    /// One sink that panics must not end the stream for the rest of the
    /// process. The registry is locked while the sinks run, so a panicking sink
    /// poisons it - and a registry that read poison as "give up" would drop
    /// every later event, including the `run-finished` a `cancelRun` is waiting
    /// for.
    #[test]
    fn a_sink_that_panics_does_not_end_the_stream() {
        let _guard = exclusively();
        // The panic is deliberate and its message is noise here; the hook goes
        // back immediately after, so a genuine panic still reports itself.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let bad = register(Box::new(|_| panic!("a sink that panics")));
        let panicked =
            std::panic::catch_unwind(|| notice("notice.run.finished", serde_json::json!({}), "info"));
        std::panic::set_hook(hook);
        assert!(panicked.is_err(), "the sink did not panic and nothing was poisoned");
        assert!(unregister(bad), "a poisoned registry would not give the sink up");

        let collected = Collected::new();
        notice("notice.run.cancelled", serde_json::json!({}), "warn");
        assert_eq!(
            collected.keys(),
            vec!["notice.run.cancelled".to_string()],
            "the stream was dead for the rest of the process"
        );
    }

    /// The stack keys on the id, and the stream is a merge: the mock mints
    /// `notice-1` too, and two counters that know nothing about each other
    /// cannot be trusted to disagree.
    #[test]
    fn a_notice_id_is_prefixed_so_the_mocks_cannot_collide_with_it() {
        let Event::Notice { id, .. } = notice_event("notice.run.finished", serde_json::json!({}), "info")
        else {
            panic!("not a notice");
        };
        assert!(id.starts_with("backend-notice-"), "{id} could be the mock's");
    }

    /// The seam's table, field for field. A rename here is a silent break: the
    /// interface reads these names off the payload and a missing one is
    /// `undefined`, not an error.
    #[test]
    fn the_payloads_carry_the_names_the_seam_declares() {
        let started = serde_json::to_value(Event::PageStarted {
            run_id: "run-1".into(),
            chapter_id: "c1".into(),
            page_id: "c1-p001".into(),
            page_index: 0,
        })
        .unwrap();
        assert_eq!(started["type"], "page-started");
        for key in ["runId", "chapterId", "pageId", "pageIndex"] {
            assert!(started.get(key).is_some(), "page-started has no {key}");
        }

        let finished = serde_json::to_value(Event::RunFinished {
            run_id: "run-1".into(),
            chapter_id: "c1".into(),
            reason: "cancelled",
            pages_queued: 4,
            pages_cleaned: 2,
            regions_cleaned: 7,
            next_page_index: Some(2),
        })
        .unwrap();
        assert_eq!(finished["type"], "run-finished");
        for key in
            ["runId", "chapterId", "reason", "pagesQueued", "pagesCleaned", "regionsCleaned"]
        {
            assert!(finished.get(key).is_some(), "run-finished has no {key}");
        }
        assert_eq!(finished["nextPageIndex"], 2);

        let notice = serde_json::to_value(Event::Notice {
            id: "notice-1".into(),
            key: "notice.run.finished".into(),
            params: serde_json::json!({ "pages": 2, "regions": 7 }),
            tone: "info",
        })
        .unwrap();
        assert_eq!(notice["type"], "notice");
        assert_eq!(notice["params"]["pages"], 2);
        assert_eq!(notice["tone"], "info");
    }

    /// `page-started` is a fifth event type, not a flag on something else.
    /// A payload that folded it into `region-done` would be a quiet
    /// reversal of that, so the discriminators are asserted distinct.
    #[test]
    fn page_started_is_its_own_event_type() {
        let types: Vec<String> = [
            Event::PageStarted {
                run_id: "r".into(),
                chapter_id: "c".into(),
                page_id: "p".into(),
                page_index: 0,
            },
            Event::RunFinished {
                run_id: "r".into(),
                chapter_id: "c".into(),
                reason: "completed",
                pages_queued: 0,
                pages_cleaned: 0,
                regions_cleaned: 0,
                next_page_index: None,
            },
            Event::Notice {
                id: "n".into(),
                key: "k".into(),
                params: serde_json::json!({}),
                tone: "info",
            },
        ]
        .iter()
        .map(|event| serde_json::to_value(event).unwrap()["type"].as_str().unwrap().to_owned())
        .collect();
        assert_eq!(types, vec!["page-started", "run-finished", "notice"]);
    }
}
