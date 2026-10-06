//! The undo journal: delta commands on disk, beside the job they belong to.
//!
//! Undo used to be
//! a stack of JavaScript closures in `src/lib/model/history.js` - one per edit,
//! each holding two whole region snapshots, all of it lost when the window
//! closed. Two things were wrong with that and only one of them was the
//! forgetting:
//!
//! 1. **It could not survive a page leaving RAM.** A closure over a region on
//!    page 140 keeps that region alive for as long as the history does, which is
//!    the whole session. The three-page
//!    window evicts page 140's regions
//!    and the closure quietly kept its own copy - the eviction gave back
//!    nothing.
//! 2. **It did not survive a restart**, so a chapter reopened after a crash had
//!    a manifest full of edits and no way to take any of them back.
//!
//! So the history is *data*, it lives on disk, and the interface holds one
//! `{seq, label}` pair per entry - enough to say what Undo would reverse, and
//! nothing else. The payload is read back one entry at a time, at the moment it
//! is replayed. **RAM is flat in edit count**; disk is not, and disk is what
//! this application has.
//!
//! ## Where it lives
//!
//! `<job>.mtclean.d/history.json` - the sidecar directory, alongside the masks
//! and patch buffers a delta refers to. That is deliberate: an entry references
//! a region id and a mask id, never pixels, and the pixels those ids name are
//! already in this directory under the soft-delete discipline
//! `library::set_mask_visible` describes. A journal that carried its own copy of
//! anything would be a second opinion about what a region is.
//!
//! ## Why the whole file is rewritten
//!
//! An append-only log is the obvious shape and it is the wrong one here: undo
//! moves a cursor backwards and the next edit **truncates the future**, which an
//! append-only file cannot express without a compaction pass nobody would ever
//! test. The journal is capped at [`JOURNAL_LIMIT`] entries, each of them a few
//! hundred bytes, so the whole document is tens of kilobytes and the rewrite is
//! one `write_atomic` - the same temp-fsync-rename every other write in this
//! application uses.
//!
//! The cap is a **behaviour change**: the session-only history was
//! unlimited, and an unlimited *persisted* history is an unbounded term in a
//! budget whose whole claim is that it is bounded.

use std::path::{Path, PathBuf};

use cleaner_core::project::{buffers, sidecar_dir};
use serde::{Deserialize, Serialize};

/// The format this build writes. Read is tolerant of a missing file and of a
/// file it cannot parse; both answer an empty journal, because a history nobody
/// can read is a history, not a corrupt chapter, and refusing to open the
/// chapter over it would be the worse failure by a wide margin.
pub const JOURNAL_VERSION: u32 = 1;

/// How many entries a chapter keeps. Oldest first out.
pub const JOURNAL_LIMIT: usize = 500;

/// The file, inside `<job>.mtclean.d/`.
pub const JOURNAL_FILE: &str = "history.json";

/// One side of a delta: what the region looked like before the edit, or after
/// it.
///
/// `present` is the only field the core actually acts on -
/// `library::restore_region` turns a persisted `visible` flag on or off and
/// reads nothing else - and it is the field that makes a *creation* undoable,
/// since "it was not there" has no region to describe.
///
/// `region` is the seam's `ApiRegion` as the interface last saw it: id, bbox,
/// engine, outcome, provenance, and a **mask reference**. It is metadata and it
/// is never pixels - the mask and the patch it names are in the same sidecar
/// directory this file sits in. It is carried because the interface has to put
/// something back into the open chapter without waiting for a re-read of the
/// manifest, and because a backend that keeps its whole history in memory (the
/// mock, and any fixture backend) restores from it directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Side {
    /// Whether the region existed at all in this state.
    pub present: bool,
    /// The status of the page the region sits on, in this state. Some region
    /// edits move the page - cleaning a gate-skipped region on an unclean page,
    /// deleting the last mask on a cleaned one - and an undo that put the region
    /// back without its page's status would leave a full track under a "not
    /// cleaned" mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_status: Option<String>,
    /// The region's own metadata, or `null` when `present` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<serde_json::Value>,
}

/// One undoable edit, as data rather than as a pair of closures.
///
/// `op` is the vocabulary, and there is exactly one verb in it today -
/// `region-state`, which every editing surface already funnels through
/// (`maskactions`, `toolapply`, `drawing`). It is a field rather than an
/// implied constant so that a second verb can be added without a second file
/// format, and so an entry written by a later build is recognisably *not*
/// something this one knows how to replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    /// Monotonic within a chapter. Identifies an entry across a reload, which
    /// is what lets the interface hold labels alone and ask for a payload by
    /// number.
    pub seq: u64,
    /// The i18n key the Undo/Redo tooltip renders. No English crosses the seam,
    /// here or anywhere.
    pub label: String,
    /// The verb. `region-state` today.
    pub op: String,
    /// Which region the delta is about.
    pub region_id: String,
    pub before: Side,
    pub after: Side,
}

/// What a delta looks like on the way in, before the journal numbers it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewDelta {
    pub label: String,
    pub op: String,
    pub region_id: String,
    pub before: Side,
    pub after: Side,
}

/// The journal as it sits on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub version: u32,
    /// Zero is the legacy native-page payload; one is oriented display geometry.
    #[serde(default)]
    pub coordinate_version: u8,
    /// How many of `entries` are in the **past**. Everything at or after this
    /// index is the redo stack, youngest last. A cursor rather than two vectors
    /// because the two vectors have to be written and read as one document
    /// anyway, and one number cannot disagree with itself.
    pub cursor: usize,
    /// The next `seq` to hand out. Kept explicitly so that trimming the oldest
    /// entries never re-issues a number.
    pub next_seq: u64,
    pub entries: Vec<Delta>,
}

impl Default for Journal {
    fn default() -> Journal {
        Journal {
            version: JOURNAL_VERSION,
            coordinate_version: 1,
            cursor: 0,
            next_seq: 1,
            entries: Vec::new(),
        }
    }
}

impl Journal {
    /// Record an edit that has **already happened**, exactly as the in-memory
    /// history did: the future is dropped, because once a new action happens the
    /// old one is no longer reachable.
    pub fn push(&mut self, delta: NewDelta) -> Delta {
        self.entries.truncate(self.cursor);
        let entry = Delta {
            seq: self.next_seq,
            label: delta.label,
            op: delta.op,
            region_id: delta.region_id,
            before: delta.before,
            after: delta.after,
        };
        self.next_seq += 1;
        self.entries.push(entry.clone());
        // Oldest out, and the cursor moves with them: an entry that has left the
        // journal has left the past, and a cursor that stayed put would count
        // entries that are no longer there.
        while self.entries.len() > JOURNAL_LIMIT {
            self.entries.remove(0);
            self.cursor = self.cursor.saturating_sub(1);
        }
        self.cursor = self.entries.len();
        entry
    }

    /// Step the cursor back one and answer with the entry to reverse.
    pub fn undo(&mut self) -> Option<Delta> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        self.entries.get(self.cursor).cloned()
    }

    /// Step the cursor forward one and answer with the entry to repeat.
    pub fn redo(&mut self) -> Option<Delta> {
        let entry = self.entries.get(self.cursor).cloned()?;
        self.cursor += 1;
        Some(entry)
    }

    /// The labels the interface holds: one short pair per entry, and never a
    /// payload. This is the whole of what undo costs in RAM.
    pub fn view(&self) -> HistoryView {
        HistoryView {
            cursor: self.cursor,
            entries: self
                .entries
                .iter()
                .map(|entry| HistoryLabel {
                    seq: entry.seq,
                    label: entry.label.clone(),
                })
                .collect(),
        }
    }
}

/// One entry, as the interface holds it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryLabel {
    pub seq: u64,
    pub label: String,
}

/// The journal's index - cursor and labels, no payloads.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryView {
    pub cursor: usize,
    pub entries: Vec<HistoryLabel>,
}

/// One step of undo or redo: where the cursor now is, and what to replay.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStep {
    pub cursor: usize,
    pub entry: Option<Delta>,
}

/// `<job>.mtclean` → `<job>.mtclean.d/history.json`.
pub fn journal_path(job: &Path) -> PathBuf {
    sidecar_dir(job).join(JOURNAL_FILE)
}

/// Read a chapter's journal.
///
/// A journal that is not there, or that cannot be parsed, is an **empty**
/// journal. Neither is an error the user can act on and neither is a reason to
/// refuse a chapter: the manifest is the record of what was done, and the
/// journal is only the record of how to take it back.
pub fn load(job: &Path) -> Journal {
    let path = journal_path(job);
    let Ok(bytes) = std::fs::read(&path) else {
        return Journal::default();
    };
    let mut journal = serde_json::from_slice::<Journal>(&bytes).unwrap_or_default();
    if journal.coordinate_version == 0 {
        if let Ok(opened) = cleaner_core::project::Job::open(job) {
            for entry in &mut journal.entries {
                let record = opened
                    .project
                    .patches
                    .iter()
                    .find(|p| p.id == entry.region_id);
                let source_idx = record.map(|p| p.source_idx).or_else(|| {
                    let suffix = entry.region_id.rsplit_once("-p")?.1;
                    let position = suffix
                        .split('-')
                        .next()?
                        .parse::<usize>()
                        .ok()?
                        .checked_sub(1)?;
                    opened.project.strip.order.get(position).copied()
                });
                let Some(source) = source_idx.and_then(|i| opened.project.sources.get(i)) else {
                    continue;
                };
                if source.orientation.0 == 1
                    && opened.project.sources.iter().all(|s| s.orientation.0 == 1)
                {
                    continue;
                }
                for side in [&mut entry.before, &mut entry.after] {
                    let Some(region) = side.region.as_mut() else {
                        continue;
                    };
                    let Some(bbox) = region.get("bbox") else {
                        continue;
                    };
                    let Some([x, y, w, h]) = ["x", "y", "w", "h"]
                        .map(|key| bbox.get(key).and_then(|n| n.as_f64()))
                        .into_iter()
                        .collect::<Option<Vec<_>>>()
                        .and_then(|v| <[f64; 4]>::try_from(v).ok())
                    else {
                        continue;
                    };
                    let native = cleaner_core::mask::Rect::new(
                        (x * source.w as f64 / 100.0).round() as i64,
                        (y * source.h as f64 / 100.0).round() as i64,
                        (w * source.w as f64 / 100.0).round().max(0.0) as u32,
                        (h * source.h as f64 / 100.0).round().max(0.0) as u32,
                    );
                    let display = if let Some(record) = record {
                        let mut historical = record.clone();
                        historical.native_parts.clear();
                        historical.bbox = native;
                        if let Some(params)=historical.provenance.params_snapshot.as_object_mut() { params.remove("layer"); }
                        cleaner_core::project::orientation::display_bbox(
                            &opened.project,
                            &historical,
                        )
                    } else {
                        source.orientation.rect(native, source.w, source.h)
                    };
                    if let Some(layer)=region.get_mut("mask").and_then(|mask|mask.get_mut("layer")) {
                        if let Ok(style)=serde_json::from_value::<cleaner_core::patch::LayerStyle>(layer.clone()) {
                            *layer=serde_json::to_value(cleaner_core::project::orientation::orient_layer_style(style,source.orientation)).unwrap_or_default();
                        }
                    }
                    let (width, height) = source.orientation.size(source.w, source.h);
                    region["bbox"] = serde_json::json!({"x":display.x as f64/width as f64*100.0,"y":display.y as f64/height as f64*100.0,"w":display.w as f64/width as f64*100.0,"h":display.h as f64/height as f64*100.0});
                }
            }
            journal.coordinate_version = 1;
        }
    }
    journal
}

/// Write a chapter's journal, temp-fsync-rename like everything else in the
/// sidecar.
pub fn save(job: &Path, journal: &Journal) -> Result<(), String> {
    let path = journal_path(job);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec(journal).map_err(|e| e.to_string())?;
    buffers::write_atomic(&path, &bytes).map_err(|e| e.to_string())
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

/// Everything below takes the job's own lock, for the reason
/// `library::set_mask_visible` states: one writer per job. The journal is a
/// different file from the manifest and a region edit writes both, so a push
/// that interleaved with a run's flush would be a journal describing a manifest
/// that had moved underneath it.
fn with_journal<T>(
    app: &tauri::AppHandle,
    chapter_id: &str,
    work: impl FnOnce(&mut Journal) -> T,
) -> Result<T, String> {
    let job = crate::library::resolve_chapter(app, chapter_id)?;
    let _lock = crate::run::lock_job(&job)?;
    let mut journal = load(&job);
    let answer = work(&mut journal);
    save(&job, &journal)?;
    Ok(answer)
}

#[tauri::command]
pub async fn history_load(
    app: tauri::AppHandle,
    chapter_id: String,
) -> Result<HistoryView, String> {
    crate::library::blocking(move || {
        let job = crate::library::resolve_chapter(&app, &chapter_id)?;
        Ok(load(&job).view())
    })
    .await
}

#[tauri::command]
pub async fn history_push(
    app: tauri::AppHandle,
    chapter_id: String,
    entry: NewDelta,
) -> Result<HistoryView, String> {
    crate::library::blocking(move || {
        with_journal(&app, &chapter_id, |journal| {
            journal.push(entry);
            journal.view()
        })
    })
    .await
}

/// One step, in the direction named. The cursor moves **on disk** before the
/// interface replays anything, which is what makes an undo interrupted by a
/// crash resolve the same way twice: the manifest edit is the thing that may or
/// may not have happened, and re-applying a delta to a manifest already in that
/// state is a no-op by construction (`set_mask_visible` answers "it is as you
/// asked" rather than refusing).
#[tauri::command]
pub async fn history_move(
    app: tauri::AppHandle,
    chapter_id: String,
    direction: String,
) -> Result<HistoryStep, String> {
    crate::library::blocking(move || {
        with_journal(&app, &chapter_id, |journal| {
            let entry = match direction.as_str() {
                "undo" => journal.undo(),
                "redo" => journal.redo(),
                _ => None,
            };
            HistoryStep {
                cursor: journal.cursor,
                entry,
            }
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(label: &str, region: &str) -> NewDelta {
        NewDelta {
            label: label.to_owned(),
            op: "region-state".to_owned(),
            region_id: region.to_owned(),
            before: Side {
                present: true,
                page_status: Some("cleaned".into()),
                region: None,
            },
            after: Side {
                present: false,
                page_status: Some("unclean".into()),
                region: None,
            },
        }
    }

    #[test]
    fn a_push_numbers_the_entry_and_moves_the_cursor_to_the_end() {
        let mut journal = Journal::default();
        let first = journal.push(delta("masks.command.deleteMask", "r1"));
        let second = journal.push(delta("masks.command.rerunMask", "r2"));
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
        assert_eq!(journal.cursor, 2);
        assert_eq!(journal.view().entries.len(), 2);
    }

    /// The interface's copy is labels and nothing else - the payload never
    /// leaves this file until it is replayed.
    #[test]
    fn the_view_carries_no_payload() {
        let mut journal = Journal::default();
        journal.push(delta("masks.command.deleteMask", "r1"));
        let view = journal.view();
        assert_eq!(
            view.entries[0],
            HistoryLabel {
                seq: 1,
                label: "masks.command.deleteMask".into()
            }
        );
    }

    #[test]
    fn undo_and_redo_walk_the_same_entries() {
        let mut journal = Journal::default();
        journal.push(delta("a", "r1"));
        journal.push(delta("b", "r2"));

        assert_eq!(journal.undo().unwrap().label, "b");
        assert_eq!(journal.cursor, 1);
        assert_eq!(journal.undo().unwrap().label, "a");
        assert_eq!(journal.cursor, 0);
        assert!(
            journal.undo().is_none(),
            "the bottom of the past is not a step"
        );

        assert_eq!(journal.redo().unwrap().label, "a");
        assert_eq!(journal.redo().unwrap().label, "b");
        assert!(journal.redo().is_none());
    }

    /// The rule the in-memory history had and the file has to keep: once a new
    /// action happens the old future is unreachable.
    #[test]
    fn a_push_after_an_undo_drops_the_future() {
        let mut journal = Journal::default();
        journal.push(delta("a", "r1"));
        journal.push(delta("b", "r2"));
        journal.undo();
        journal.push(delta("c", "r3"));

        let labels: Vec<_> = journal
            .view()
            .entries
            .iter()
            .map(|e| e.label.clone())
            .collect();
        assert_eq!(labels, vec!["a", "c"]);
        assert_eq!(journal.cursor, 2);
        assert!(journal.redo().is_none());
    }

    /// Oldest out, and no number reissued - a `seq` that came back would make
    /// two different edits the same entry to any caller holding the old one.
    #[test]
    fn the_journal_is_capped_and_never_reissues_a_number() {
        let mut journal = Journal::default();
        for n in 0..JOURNAL_LIMIT + 10 {
            journal.push(delta(&format!("e{n}"), "r1"));
        }
        assert_eq!(journal.entries.len(), JOURNAL_LIMIT);
        assert_eq!(journal.cursor, JOURNAL_LIMIT);
        assert_eq!(journal.entries[0].label, "e10");
        assert_eq!(journal.next_seq, JOURNAL_LIMIT as u64 + 11);
    }

    #[test]
    fn a_journal_survives_a_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("mtclean-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let job = dir.join("chapter.mtclean");

        let mut journal = Journal::default();
        journal.push(delta("masks.command.deleteMask", "r1"));
        save(&job, &journal).unwrap();

        let read = load(&job);
        assert_eq!(read.cursor, 1);
        assert_eq!(read.entries[0].region_id, "r1");
        assert_eq!(
            read.entries[0].before.page_status.as_deref(),
            Some("cleaned")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A journal nobody can parse is an empty journal, not a broken chapter.
    #[test]
    fn an_unreadable_journal_reads_as_empty() {
        let dir = std::env::temp_dir().join(format!("mtclean-history-bad-{}", std::process::id()));
        std::fs::create_dir_all(sidecar_dir(&dir.join("chapter.mtclean"))).unwrap();
        let job = dir.join("chapter.mtclean");
        std::fs::write(journal_path(&job), b"{ not json").unwrap();

        let read = load(&job);
        assert_eq!(read.cursor, 0);
        assert!(read.entries.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
