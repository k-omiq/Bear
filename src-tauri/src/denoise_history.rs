//! A chapter's page denoise history, as the chapter list shows it.
//!
//! - [`summary`] is the line the chapter list carries for every chapter
//!   (`ApiChapter.denoiseHistory`): how many runs are remembered, when the
//!   newest was, and whether one was taken as the pages. It reads the rows and
//!   nothing else, so a listing pays nothing for it.
//! - [`denoise_history`] is the whole history of one chapter, newest run
//!   first: the preset, where it ran, whether it was made from cleaned pages,
//!   and per page whether its file is still on disk and whether it was taken.
//! - [`denoise_compare_image`] answers one side of the compare view: a run's
//!   denoised file for a page, or the raw page that file was made from.
//!
//! The rows are `Project::denoised`, written by `cloud_denoise::record_written`
//! after a local or a cloud run and marked taken by `Job::replace_source`. A
//! run is the rows sharing one `created`, and `created` is how the interface
//! names a run.
//!
//! The image command is the trust boundary: it takes a chapter, a page and a
//! run, never a path, and serves only a file the manifest recorded for that
//! page and run, or that page's own source.

use std::path::{Path, PathBuf};

use cleaner_core::cloud_denoise_wire::PresetTarget;
use cleaner_core::project::{DenoisedPage, Job, Project};
use serde::{Deserialize, Serialize};

use crate::library::Library;

/// What the chapter list shows of a chapter's denoise history.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DenoiseSummary {
    /// Runs remembered, at most `MAX_DENOISE_RUNS`.
    pub runs: u32,
    /// The newest run's `created`, seconds since the epoch.
    pub latest: u64,
    /// Whether any run's file was taken as a page.
    pub taken: bool,
}

/// The summary for one chapter's manifest, `None` when it was never denoised.
pub(crate) fn summary(project: &Project) -> Option<DenoiseSummary> {
    let mut runs: Vec<u64> = project.denoised.iter().map(|row| row.created).collect();
    runs.sort_unstable();
    runs.dedup();
    Some(DenoiseSummary {
        runs: runs.len() as u32,
        latest: *runs.last()?,
        taken: project.denoised.iter().any(|row| row.taken),
    })
}

/// One page of a run.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DenoiseRunPage {
    pub page_index: u32,
    /// Whether the file the run wrote is still on disk.
    pub exists: bool,
    /// Whether the file was taken as the page.
    pub taken: bool,
    /// Whether it is the page's current denoised file, the one "Replace pages
    /// with denoised" would take.
    pub current: bool,
    /// Whether the page had cleaning on it when it was read. Known for every
    /// row written since it was recorded; for an older row, only when the
    /// page still looks the way it did then.
    pub from_cleaned: Option<bool>,
}

/// One run, as the history lists it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DenoiseRun {
    /// The run's name, and when it was recorded: seconds since the epoch.
    pub created: u64,
    /// The shipped preset, `None` when the run did not record one.
    pub preset: Option<String>,
    pub target: Option<PresetTarget>,
    /// True when any page was read with cleaning on it, false when every page
    /// is known to have been bare, `None` when the history cannot say.
    pub from_cleaned: Option<bool>,
    /// The folder the files were written to, from the first page's file.
    pub folder: String,
    /// In reading order.
    pub pages: Vec<DenoiseRunPage>,
}

/// Every run held for the chapter, newest first. A row whose source is no
/// longer in the chapter's reading order has no page to show and is left out.
pub(crate) fn history(project: &Project) -> Vec<DenoiseRun> {
    let appearances = crate::tile::page_appearances(project);
    let position_of = |source_idx: usize| project.strip.order.iter().position(|held| *held == source_idx);
    let mut runs: Vec<DenoiseRun> = Vec::new();
    let mut rows: Vec<&DenoisedPage> = project.denoised.iter().collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.created));
    for row in rows {
        let Some(position) = position_of(row.source_idx) else { continue };
        let from_cleaned = row.from_cleaned.or_else(|| {
            // An old row: its appearance is the bare page's, or the page's
            // present one with patches on it, or neither and it cannot say.
            let bare = crate::tile::source_appearance(project, position);
            if row.appearance == bare {
                Some(false)
            } else {
                (appearances.get(position) == Some(&row.appearance)).then_some(true)
            }
        });
        let page = DenoiseRunPage {
            page_index: position as u32,
            exists: row.path.is_file(),
            taken: row.taken,
            current: project.current_denoised(row.source_idx).is_some_and(|current| std::ptr::eq(current, row)),
            from_cleaned,
        };
        match runs.last_mut() {
            Some(run) if run.created == row.created => run.pages.push(page),
            _ => runs.push(DenoiseRun {
                created: row.created,
                preset: row.preset.clone(),
                target: row.target,
                from_cleaned: None,
                folder: row.path.parent().map(|dir| dir.display().to_string()).unwrap_or_default(),
                pages: vec![page],
            }),
        }
    }
    for run in &mut runs {
        run.pages.sort_by_key(|page| page.page_index);
        run.from_cleaned = if run.pages.iter().any(|page| page.from_cleaned == Some(true)) {
            Some(true)
        } else if run.pages.iter().all(|page| page.from_cleaned == Some(false)) {
            Some(false)
        } else {
            None
        };
    }
    runs
}

/// Which side of the compare view to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompareSide {
    /// The page the run read: the source it was made from, which after a take
    /// is no longer the page's source.
    Raw,
    /// The file the run wrote.
    Denoised,
}

/// The file one side of the compare view reads, for page `page_index` of the
/// run `run`. Only a path the manifest holds: the run's own file for that
/// page, or the source it was made from, or the page's source when an old row
/// did not record one.
fn compare_path(job: &Job, page_index: u32, run: u64, side: CompareSide) -> Result<PathBuf, String> {
    let source_idx = Library::resolve_page(&job.project, page_index as usize).ok_or("denoise_page_missing")?;
    let row = job.project.denoised.iter()
        .find(|row| row.source_idx == source_idx && row.created == run)
        .ok_or("denoise_run_missing")?;
    Ok(match (side, &row.source) {
        (CompareSide::Denoised, _) => row.path.clone(),
        (CompareSide::Raw, Some(source)) => job.root().join(source),
        (CompareSide::Raw, None) => job.source_path(source_idx).ok_or("denoise_page_missing")?,
    })
}

fn compare_bytes(job_path: &Path, page_index: u32, run: u64, side: CompareSide) -> Result<Vec<u8>, String> {
    let job = Job::open_read_only(job_path).map_err(|e| e.to_string())?;
    let path = compare_path(&job, page_index, run, side)?;
    std::fs::read(&path).map_err(|e| format!("denoise_file_missing: {e}"))
}

/// The chapter's denoise history, newest run first.
#[tauri::command]
pub async fn denoise_history(app: tauri::AppHandle, chapter_id: String) -> Result<Vec<DenoiseRun>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let job_path = Library::for_app(&app).map_err(|e| e.to_string())?
            .resolve_chapter(&chapter_id).map_err(|e| e.to_string())?;
        let job = Job::open_read_only(&job_path).map_err(|e| e.to_string())?;
        Ok(history(&job.project))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One side of the compare view as the file's own bytes, raw over IPC: the
/// denoised file the run `run` wrote for page `page_index`, or the raw page it
/// was made from. Refused as `denoise_page_missing` or `denoise_run_missing`
/// for a page or run the manifest does not hold, and as
/// `denoise_file_missing` when the file is gone.
#[tauri::command]
pub async fn denoise_compare_image(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: u32,
    run: u64,
    side: CompareSide,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let job_path = Library::for_app(&app).map_err(|e| e.to_string())?
            .resolve_chapter(&chapter_id).map_err(|e| e.to_string())?;
        compare_bytes(&job_path, page_index, run, side).map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::image::{encode, BitDepth, ColorMode, Format, Raster};
    use cleaner_core::project::StripMode;

    fn grey(value: u8) -> Vec<u8> {
        let raster = Raster {
            width: 8, height: 6, mode: ColorMode::Gray, depth: BitDepth::Eight, icc: None, palette: None, trns: None,
            srgb_intent: None, color: Default::default(), data: vec![value; 48],
        };
        encode(&raster, Format::Png).unwrap()
    }

    /// A two-page chapter under `dir` with two runs recorded: an old row with
    /// only the four original fields for page 1 at 100, and a local run of
    /// both pages at 200 whose page 2 file has since gone.
    fn a_chapter_with_history(dir: &Path) -> PathBuf {
        let _ = std::fs::remove_dir_all(dir);
        let raws = dir.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let sources: Vec<_> = (0..2u8).map(|at| {
            let bytes = grey(200 + at);
            let path = raws.join(format!("{:03}.png", at + 1));
            std::fs::write(&path, &bytes).unwrap();
            cleaner_core::ingest::source_ref(&path, &bytes).unwrap()
        }).collect();
        let manifest = dir.join("job").join("chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(manifest.parent().unwrap(), "test", StripMode::Single, &sources);
        let mut job = Job::create(&manifest, project).unwrap();
        let bare = crate::tile::source_appearance(&job.project, 0);
        let (old, new) = (dir.join("old"), dir.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("001.png"), grey(10)).unwrap();
        std::fs::write(new.join("001.png"), grey(20)).unwrap();
        let legacy: DenoisedPage = serde_json::from_value(serde_json::json!({
            "source_idx": 0, "path": old.join("001.png"), "appearance": bare, "created": 100,
        })).unwrap();
        job.record_denoised(vec![legacy]).unwrap();
        let rows = (0..2).map(|at| DenoisedPage {
            source_idx: at,
            path: new.join(format!("{:03}.png", at + 1)),
            appearance: crate::tile::source_appearance(&job.project, at),
            created: 200,
            preset: Some("waifu2x-scan-4x-n2".into()),
            target: Some(PresetTarget::Local),
            from_cleaned: Some(false),
            source: Some(job.project.sources[at].rel_path.clone()),
            taken: false,
        }).collect();
        job.record_denoised(rows).unwrap();
        manifest
    }

    #[test]
    fn the_history_lists_runs_newest_first_with_what_each_page_has_left() {
        let dir = std::env::temp_dir().join(format!("mc-denoise-history-{}", std::process::id()));
        let manifest = a_chapter_with_history(&dir);
        let project = Job::open(&manifest).unwrap().project;
        assert_eq!(summary(&project), Some(DenoiseSummary { runs: 2, latest: 200, taken: false }));
        let mut never = project.clone();
        never.denoised.clear();
        assert_eq!(summary(&never), None);

        let runs = history(&project);
        assert_eq!(runs.iter().map(|run| run.created).collect::<Vec<_>>(), [200, 100]);
        let [newest, oldest] = runs.as_slice() else { unreachable!() };
        assert_eq!((newest.preset.as_deref(), newest.target), (Some("waifu2x-scan-4x-n2"), Some(PresetTarget::Local)));
        assert_eq!(newest.from_cleaned, Some(false));
        assert_eq!(newest.folder, dir.join("new").display().to_string());
        let pages: Vec<(u32, bool, bool)> = newest.pages.iter().map(|page| (page.page_index, page.exists, page.current)).collect();
        assert_eq!(pages, [(0, true, true), (1, false, true)], "page 2's file is gone");
        assert_eq!((oldest.preset.as_deref(), oldest.target), (None, None), "an old row names no preset");
        assert_eq!(oldest.from_cleaned, Some(false), "read from its appearance, which is the bare page's");
        assert!(!oldest.pages[0].current);

        let json = serde_json::to_value(&runs).unwrap();
        assert_eq!(json[0]["target"], "local");
        assert_eq!(json[0]["pages"][0]["pageIndex"], 0);
        assert_eq!(json[0]["fromCleaned"], false);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The compare view reads only what the manifest holds: the run's file,
    /// or the raw page it was made from, which after a take is the old source
    /// and not the page's new one.
    #[test]
    fn compare_serves_recorded_files_and_refuses_the_rest() {
        let dir = std::env::temp_dir().join(format!("mc-denoise-compare-{}", std::process::id()));
        let manifest = a_chapter_with_history(&dir);
        assert_eq!(compare_bytes(&manifest, 0, 200, CompareSide::Denoised).unwrap(), grey(20));
        assert_eq!(compare_bytes(&manifest, 0, 100, CompareSide::Denoised).unwrap(), grey(10));
        assert_eq!(compare_bytes(&manifest, 0, 200, CompareSide::Raw).unwrap(), grey(200));
        assert_eq!(compare_bytes(&manifest, 0, 100, CompareSide::Raw).unwrap(), grey(200), "an old row reads the source");
        assert_eq!(compare_bytes(&manifest, 1, 100, CompareSide::Denoised).unwrap_err(), "denoise_run_missing");
        assert_eq!(compare_bytes(&manifest, 0, 150, CompareSide::Raw).unwrap_err(), "denoise_run_missing");
        assert_eq!(compare_bytes(&manifest, 7, 200, CompareSide::Raw).unwrap_err(), "denoise_page_missing");
        assert!(compare_bytes(&manifest, 1, 200, CompareSide::Denoised).unwrap_err().starts_with("denoise_file_missing"));

        {
            let mut job = Job::open(&manifest).unwrap();
            job.replace_source(0, &grey(20)).unwrap();
            job.flush().unwrap();
        }
        let project = Job::open(&manifest).unwrap().project;
        assert_eq!(summary(&project).map(|summary| summary.taken), Some(true));
        let runs = history(&project);
        assert!(runs[0].pages[0].taken && !runs[0].pages[0].current);
        assert_eq!(compare_bytes(&manifest, 0, 200, CompareSide::Raw).unwrap(), grey(200), "the raw page, not the taken file");
        assert_eq!(compare_bytes(&manifest, 0, 100, CompareSide::Raw).unwrap(), grey(200), "for the old row too");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A cloud run records the preset its recipe is, so the history can name it.
    #[test]
    fn a_cloud_recipe_is_named_by_the_preset_it_is() {
        use crate::inference::cloud_denoise::preset_of;
        for preset in cleaner_core::cloud_denoise_wire::PRESETS.iter() {
            assert_eq!(preset_of(&preset.recipe()), Some(preset.id));
        }
        assert_eq!(preset_of(&serde_json::from_str(r#"{"schema":1,"steps":[]}"#).unwrap()), None);
    }

    #[test]
    fn a_side_is_named_in_lowercase() {
        assert_eq!(serde_json::from_str::<CompareSide>("\"raw\"").unwrap(), CompareSide::Raw);
        assert_eq!(serde_json::from_str::<CompareSide>("\"denoised\"").unwrap(), CompareSide::Denoised);
        assert!(serde_json::from_str::<CompareSide>("\"/etc/passwd\"").is_err());
    }
}
