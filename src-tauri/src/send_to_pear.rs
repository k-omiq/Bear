//! Send to Pear: bring the cleaner's existing titles across
//! (`docs/shared-data-folder.md` section 8.4.1).
//!
//! The cleaner sends and Pear matches. This side lists its titles, then
//! writes one message per chapter through the Save path
//! ([`crate::exporting::send_chapter`]) with `"send": "bulk"`, in the
//! background, one chapter at a time. It can be stopped between chapters;
//! the messages already written stay valid. A chapter with no cleaned page
//! is never sent: Pear would get raws only, and a later Save sends it.
//!
//! Each chapter's `last_sent_at` in the cleaner's own index says when it was
//! last handed over, by a Save or by this. A title with a sent chapter is
//! "sent before" (the cleaner cannot know whether Pear filed it) and sends
//! only its changed and never-sent chapters, unless "Send all again".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::exporting::{send_chapter, SendKind};
use crate::library::{ApiProject, Library};

/// One row of the title list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleRow {
    pub project_id: String,
    pub name: String,
    /// All its chapters.
    pub chapters: usize,
    /// Chapters with at least one cleaned page: the ones that can be sent.
    pub cleaned: usize,
    /// Chapters handed to Pear before, by a Save or a send.
    pub sent: usize,
    /// Cleaned chapters a send without "Send all again" would write: never
    /// sent, or changed since.
    pub pending: usize,
}

/// A chapter with a cleaned page, as Home counts one.
fn has_cleaned_page(chapter: &crate::library::ApiChapter) -> bool {
    chapter.pages.iter().any(|page| page.status == "cleaned")
}

/// The title list, in the library's own order.
pub fn titles(lib: &Library) -> Result<Vec<TitleRow>, String> {
    let index = lib.index().map_err(|e| e.to_string())?;
    let built: Vec<ApiProject> = lib.list_projects().map_err(|e| e.to_string())?;
    Ok(index
        .projects
        .iter()
        .map(|project| {
            let api = built.iter().find(|p| p.id == project.id);
            let cleaned = |id: &str| api.and_then(|p| p.chapters.iter().find(|c| c.id == id)).is_some_and(has_cleaned_page);
            TitleRow {
                project_id: project.id.clone(),
                name: project.name.clone(),
                chapters: project.chapters.len(),
                cleaned: project.chapters.iter().filter(|c| cleaned(&c.id)).count(),
                sent: project.chapters.iter().filter(|c| c.last_sent_at.is_some()).count(),
                pending: project.chapters.iter().filter(|c| cleaned(&c.id) && lib.changed_since_sent(&project.id, c)).count(),
            }
        })
        .collect())
}

/// The chapters a send of `project_ids` writes, in title then chapter order
/// (oldest chapter first, as Pear lists them): every cleaned chapter with
/// `again`, else only those never sent or changed since.
pub fn plan(lib: &Library, project_ids: &[String], again: bool) -> Result<Vec<String>, String> {
    let index = lib.index().map_err(|e| e.to_string())?;
    let built: Vec<ApiProject> = lib.list_projects().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for project in index.projects.iter().filter(|p| project_ids.contains(&p.id)) {
        let api = built.iter().find(|p| p.id == project.id);
        let mut chapters: Vec<_> = project.chapters.iter().collect();
        chapters.sort_by(|a, b| a.number.cmp(&b.number).then(a.created.cmp(&b.created)));
        for chapter in chapters {
            let cleaned = api.and_then(|p| p.chapters.iter().find(|c| c.id == chapter.id)).is_some_and(has_cleaned_page);
            if cleaned && (again || lib.changed_since_sent(&project.id, chapter)) {
                out.push(chapter.id.clone());
            }
        }
    }
    Ok(out)
}

/// Where a send is, for "Sending 40 of 63 chapters".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub running: bool,
    /// Chapters looked at so far, sent or not.
    pub done: usize,
    pub total: usize,
    pub sent: usize,
    /// Chapters that could not be sent (a run holds one, say).
    pub failed: usize,
    /// The send was stopped before its end.
    pub stopped: bool,
}

static PROGRESS: Mutex<Progress> = Mutex::new(Progress { running: false, done: 0, total: 0, sent: 0, failed: 0, stopped: false });
static STOP: AtomicBool = AtomicBool::new(false);

fn progress() -> std::sync::MutexGuard<'static, Progress> {
    PROGRESS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether a send is going. A move of the data folder waits for it.
pub fn running() -> bool {
    progress().running
}

/// Send `chapters` one by one. `root` is asked again before each one, so a
/// folder that stopped being Shared ends the send; `stop` is looked at
/// between chapters. What the progress says is updated as it goes.
pub fn run(lib: &Library, chapters: &[String], mut root: impl FnMut() -> Option<std::path::PathBuf>, app_version: &str, stop: &AtomicBool) {
    for chapter in chapters {
        if stop.load(Ordering::SeqCst) {
            progress().stopped = true;
            break;
        }
        let Some(root) = root() else {
            progress().stopped = true;
            break;
        };
        let outcome = send_chapter(lib, &root, chapter, false, app_version, SendKind::Bulk);
        let mut at = progress();
        at.done += 1;
        match outcome {
            Ok(result) if result.status == "saved" => at.sent += 1,
            Ok(result) => {
                eprintln!("manga-cleaner: send to Pear: {chapter}: {:?} {:?}", result.reason_key, result.detail);
                at.failed += 1;
            }
            Err(error) => {
                eprintln!("manga-cleaner: send to Pear: {chapter}: {error}");
                at.failed += 1;
            }
        }
    }
}

#[tauri::command]
pub async fn send_to_pear_titles(app: tauri::AppHandle) -> Result<Vec<TitleRow>, String> {
    crate::library::blocking(move || {
        let lib = Library::for_app(&app).map_err(|e| e.to_string())?;
        titles(&lib)
    })
    .await
}

/// Start a send of the titles named. Refused outside Shared mode and while
/// another send runs. Answers at once with the progress; the chapters are
/// written on a thread of their own. The always-copy rule of `next-to-raws`
/// is not needed here: a bulk send writes the data folder only.
#[tauri::command]
pub async fn send_to_pear_start(app: tauri::AppHandle, project_ids: Vec<String>, again: bool) -> Result<Progress, String> {
    crate::library::blocking(move || {
        if crate::data_folder::shared_root(&app).is_none() {
            return Err("Send to Pear needs the shared data folder.".into());
        }
        let lib = Library::for_app(&app).map_err(|e| e.to_string())?;
        let chapters = plan(&lib, &project_ids, again)?;
        {
            let mut at = progress();
            if at.running {
                return Err("A send to Pear is already going.".into());
            }
            *at = Progress { running: true, total: chapters.len(), ..Progress::default() };
        }
        STOP.store(false, Ordering::SeqCst);
        let version = app.package_info().version.to_string();
        std::thread::spawn(move || {
            run(&lib, &chapters, || crate::data_folder::shared_root(&app), &version, &STOP);
            let done = {
                let mut at = progress();
                at.running = false;
                at.clone()
            };
            crate::events::notice(
                if done.failed > 0 {
                    "notice.sendToPear.failed"
                } else if done.stopped {
                    "notice.sendToPear.stopped"
                } else {
                    "notice.sendToPear.done"
                },
                serde_json::json!({ "count": done.sent, "failed": done.failed }),
                if done.failed > 0 { "warn" } else { "info" },
            );
        });
        Ok(progress().clone())
    })
    .await
}

#[tauri::command]
pub fn send_to_pear_progress() -> Progress {
    progress().clone()
}

/// Stop after the chapter being written. The messages already written stay.
#[tauri::command]
pub fn send_to_pear_stop() -> Progress {
    STOP.store(true, Ordering::SeqCst);
    progress().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::image::{fixtures, Format};
    use cleaner_core::project::{Job, Project, StripMode};
    use std::path::{Path, PathBuf};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Scratch {
            use std::sync::atomic::AtomicU32;
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join("cleaner-send-to-pear")
                .join(format!("{name}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A data folder whose `Cleaner/` holds one title, "Dungeon streamer",
    /// with three chapters: c1 cleaned, c2 cleaned, c3 untouched; and a
    /// second title "Nothing yet" with one untouched chapter.
    fn library(s: &Scratch) -> (PathBuf, Library) {
        let root = s.0.join("Manga Library");
        let lib_root = root.join("Cleaner");
        let png = cleaner_core::image::encode(&fixtures::by_name("rgb8").raster, Format::Png).unwrap();
        let chapter = |project: &str, id: &str, cleaned: bool| {
            let pages = lib_root.join(project).join(format!("{id}.mtclean.d/pages"));
            std::fs::create_dir_all(&pages).unwrap();
            std::fs::write(pages.join("001.png"), &png).unwrap();
            let source = cleaner_core::ingest::source_ref(&pages.join("001.png"), &png).unwrap();
            let manifest = lib_root.join(project).join(format!("{id}.mtclean"));
            let mut job = Job::create(&manifest, Project::new(manifest.parent().unwrap(), "test", StripMode::Single, &[source])).unwrap();
            if cleaned {
                // A page a run examined and found finished: Home's "cleaned".
                job.project.examined.push(0);
                job.flush().unwrap();
            }
        };
        chapter("p4", "c1", true);
        chapter("p4", "c2", true);
        chapter("p4", "c3", false);
        chapter("p5", "c6", false);
        let row = |id: &str, number: u32, label: Option<&str>| {
            serde_json::json!({ "id": id, "name": format!("Chapter {number}"), "number": number, "label": label,
                "source_path": null, "created": number, "last_opened": 0 })
        };
        let index = serde_json::json!({
            "version": 1, "next_id": 10,
            "projects": [
                { "id": "p4", "name": "Dungeon streamer", "mode": "single", "source_path": null, "created": 0, "last_opened": 0,
                  "chapters": [row("c3", 3, None), row("c2", 2, None), row("c1", 1, None)] },
                { "id": "p5", "name": "Nothing yet", "mode": "single", "source_path": null, "created": 0, "last_opened": 0,
                  "chapters": [row("c6", 1, None)] },
            ]
        });
        std::fs::write(lib_root.join("index.json"), serde_json::to_vec_pretty(&index).unwrap()).unwrap();
        std::fs::write(root.join("manga-library.json"), br#"{"schema":1,"folderId":"f"}"#).unwrap();
        (root, Library::at(&lib_root))
    }

    fn messages(root: &Path) -> Vec<serde_json::Value> {
        let base = root.join("handoff/to-typesetter");
        let mut out: Vec<_> = std::fs::read_dir(base)
            .map(|d| d.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.')).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|e| serde_json::from_slice(&std::fs::read(e.path().join("handoff.json")).unwrap()).unwrap())
            .collect();
        out.sort_by_key(|m: &serde_json::Value| m["cleaner"]["chapterId"].as_str().unwrap().to_owned());
        out
    }

    #[test]
    fn the_title_list_counts_chapters_cleaned_and_sent() {
        let s = Scratch::new("titles");
        let (_, lib) = library(&s);
        let rows = titles(&lib).unwrap();
        assert_eq!(rows[0], TitleRow { project_id: "p4".into(), name: "Dungeon streamer".into(), chapters: 3, cleaned: 2, sent: 0, pending: 2 });
        assert_eq!((rows[1].chapters, rows[1].cleaned), (1, 0), "Nothing cleaned yet");
    }

    #[test]
    fn a_bulk_send_writes_cleaned_chapters_only_marks_them_sent_and_then_sends_only_changes() {
        let s = Scratch::new("bulk");
        let (root, lib) = library(&s);
        let ids = vec!["p4".to_owned(), "p5".to_owned()];
        let chapters = plan(&lib, &ids, false).unwrap();
        assert_eq!(chapters, ["c1", "c2"], "no untouched chapter, oldest first");
        let stop = AtomicBool::new(false);
        run(&lib, &chapters, || Some(root.clone()), "test", &stop);
        let sent = messages(&root);
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|m| m["send"] == "bulk"), "{sent:?}");
        assert_eq!(sent[0]["chapter"]["cleanedPages"], 1);
        let rows = titles(&lib).unwrap();
        assert_eq!((rows[0].sent, rows[0].pending), (2, 0), "sent before, nothing changed");
        assert!(plan(&lib, &ids, false).unwrap().is_empty());
        assert_eq!(plan(&lib, &ids, true).unwrap(), ["c1", "c2"], "Send all again");
        // An edit after the send makes that chapter changed.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let manifest = lib.job_path("p4", "c2");
        let mut job = Job::open(&manifest).unwrap();
        job.project.extra.insert("touched".into(), serde_json::json!(true));
        job.flush().unwrap();
        assert_eq!(plan(&lib, &ids, false).unwrap(), ["c2"]);
    }

    #[test]
    fn a_stopped_send_keeps_the_messages_already_written() {
        let s = Scratch::new("stop");
        let (root, lib) = library(&s);
        let stop = AtomicBool::new(false);
        let chapters = plan(&lib, &["p4".to_owned()], true).unwrap();
        let mut asked = 0;
        // The folder goes away after the first chapter: the send ends there.
        run(&lib, &chapters, || {
            asked += 1;
            (asked == 1).then(|| root.clone())
        }, "test", &stop);
        assert_eq!(messages(&root).len(), 1);
        stop.store(true, Ordering::SeqCst);
        run(&lib, &chapters, || Some(root.clone()), "test", &stop);
        assert_eq!(messages(&root).len(), 1, "a stop before the first chapter writes nothing");
    }
}
