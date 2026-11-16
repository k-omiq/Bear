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
//! ## Undo never leaves the page on screen
//!
//! One file per chapter, **partitioned by page**. Every entry belongs to
//! exactly one page ([`Delta::page`]), and Undo and Redo are always asked for
//! one page: Undo on page 14 takes back page 14's newest step and nothing
//! else, however many edits were made on other pages since. A chapter-wide
//! stack walked back through work on pages the user could not see, and the
//! next edit then cut that work off for good.
//!
//! So an entry on the redo side is marked ([`Delta::undone`]) rather than
//! placed after a cursor, and within one page the marked entries are always
//! that page's newest. An edit on a page drops that page's marked entries
//! only. A step that touched several pages (a Detect run, a Nano Banana
//! batch, an eraser stroke across a page boundary in the long strip) is kept
//! as one entry per page, so each page's Undo takes back its own part.
//!
//! ## Why the whole file is rewritten
//!
//! An append-only log is the obvious shape and it is the wrong one here: undo
//! marks entries and the next edit on a page **drops that page's future**,
//! which an append-only file cannot express without a compaction pass nobody
//! would ever test. The journal is capped, so the rewrite is one
//! `write_atomic` of a bounded document - the same temp-fsync-rename every
//! other write in this application uses.
//!
//! The cap is a **behaviour change**: the session-only history was
//! unlimited, and an unlimited *persisted* history is an unbounded term in a
//! budget whose whole claim is that it is bounded.
//!
//! ## The cap is a setting, and this module owns it
//!
//! `undoLimit` in `settings.json` is how many entries a chapter keeps:
//! [`UNDO_LIMIT_DEFAULT`] when it is not set, never below [`UNDO_LIMIT_MIN`]
//! and never above [`UNDO_LIMIT_MAX`]. The interface holds no copy of these
//! numbers. It asks [`undo_limit`] for them, and every index this module
//! answers with carries the limit that was applied.
//!
//! The ceiling comes from the rewrite. An entry is not the few hundred bytes
//! this comment used to claim: each side carries the region's whole record,
//! and real journals measure 2.4 to 3.5 KB an entry. A full journal is
//! therefore megabytes, written again on every edit, and
//! `a_push_is_timed_on_a_real_journal` below is the measurement the ceiling
//! was checked against. A lower setting takes effect at the next push, which
//! trims the oldest entries.

use std::path::{Path, PathBuf};

use cleaner_core::project::{buffers, sidecar_dir};
use serde::{Deserialize, Serialize};

/// The format this build writes. Read is tolerant of a missing file and of a
/// file it cannot parse; both answer an empty journal, because a history nobody
/// can read is a history, not a corrupt chapter, and refusing to open the
/// chapter over it would be the worse failure by a wide margin.
///
/// Version 1 was one stack over the chapter, steered by `cursor`. Version 2
/// gives every entry its page and an `undone` mark ([`load_for`] reads a
/// version 1 file into that shape).
pub const JOURNAL_VERSION: u32 = 2;

/// The setting that says how many entries a chapter keeps. Oldest first out.
pub const UNDO_LIMIT_KEY: &str = "undoLimit";
/// What a chapter keeps when the setting was never written.
pub const UNDO_LIMIT_DEFAULT: usize = 500;
/// Low enough to be a real choice, high enough that Undo always has a step.
pub const UNDO_LIMIT_MIN: usize = 20;
/// The whole journal is rewritten on every push. See the module docs. At this
/// size one push took 19 to 29 ms on real entries (Apple M5, internal disk,
/// 2026-10-12), against 9 to 10 ms at the default.
pub const UNDO_LIMIT_MAX: usize = 2_000;

/// A stored `undoLimit`, if it is one: a whole number inside the bounds.
pub fn valid_limit(value: &serde_json::Value) -> Option<usize> {
    let limit = usize::try_from(value.as_u64()?).ok()?;
    (UNDO_LIMIT_MIN..=UNDO_LIMIT_MAX).contains(&limit).then_some(limit)
}

/// The limit a settings snapshot asks for. Anything that is not a valid limit
/// (no key, a hand-edited file, a value from a build with other bounds) is
/// the default, because a journal with a wrong cap is a worse answer than a
/// journal with the usual one.
pub fn limit_from(settings: &serde_json::Value) -> usize {
    settings.get(UNDO_LIMIT_KEY).and_then(valid_limit).unwrap_or(UNDO_LIMIT_DEFAULT)
}

fn limit_for(app: &tauri::AppHandle) -> usize {
    let limit = limit_from(&crate::settings::read(app).unwrap_or(serde_json::Value::Null));
    LAST_LIMIT.store(limit, std::sync::atomic::Ordering::Relaxed);
    limit
}

/// The limit the setting last read as. A step the backend records itself
/// ([`record_step`]) is written where no settings file is at hand - a cloud
/// result committing, a run cleaning a page - so it trims by this. Every
/// command here that reads the setting keeps it current, and opening a
/// chapter is one of them. A limit that changed since is applied by the next
/// push that reads it.
static LAST_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(UNDO_LIMIT_DEFAULT);

/// The verb of a clean of a detected region, and of taking one back. One side
/// of the entry is the detected region and the other is its result; a replay
/// asks for whichever its side is (`library::set_region_cleaned`), so undo
/// and redo are one call with a different boolean, and a rejected result is
/// the same entry written the other way round.
pub const CLEAN_OP: &str = "clean-state";
/// The Undo tooltip of a clean the backend recorded.
pub const CLEAN_LABEL: &str = "masks.command.cleanRegion";
/// The Undo tooltip of a run's cleans recorded as one step: a Nano Banana
/// batch's, the one run that cleans on its own behalf ([`record_clean`]).
pub const CLEAN_BATCH_LABEL: &str = "nanoBanana.job.undo";
/// The Undo tooltip of a rejected result.
pub const REJECT_LABEL: &str = "masks.command.rejectResult";
/// The Undo tooltips of the edits of a page's detections and held rows
/// ([`DETECTION_OP`]).
pub const DELETE_DETECTION_LABEL: &str = "masks.command.deleteDetection";
pub const DETECTION_TYPE_LABEL: &str = "masks.command.detectionType";
pub const EDIT_DETECTION_LABEL: &str = "masks.command.editDetection";
pub const PADDING_LABEL: &str = "masks.command.detectionPadding";
pub const APPROVE_LABEL: &str = "masks.command.approveMask";
pub const DISMISS_LABEL: &str = "masks.command.deleteRegion";
pub const DETECT_LABEL: &str = "masks.command.detectPages";

/// One side of a step the backend records: the region as the manifest holds
/// it now, with its page's status. `None` for a region the page does not
/// list.
pub(crate) fn side_now(
    chapter_id: &str,
    project: &cleaner_core::project::Project,
    source_idx: usize,
    region_id: &str,
) -> Option<Side> {
    let (region, page_status) = crate::library::region_and_status(chapter_id, project, source_idx, region_id)?;
    Some(Side {
        present: true,
        page_status: Some(page_status),
        region: serde_json::to_value(region).ok(),
    })
}

/// Record a step the backend made itself, and tell the interface its index
/// moved (`history-changed`).
///
/// **The caller holds the job's lock**, and calls this right after the
/// manifest write the step describes, so the journal never describes a
/// manifest another writer has moved. A result that commits while its
/// chapter is not the open one is recorded all the same, which is why the
/// backend writes it and the interface does not.
pub(crate) fn record_step(
    job: &Path,
    chapter_id: &str,
    label: &str,
    region_id: &str,
    before: Side,
    after: Side,
) -> Result<HistoryView, String> {
    push_step(job, chapter_id, CLEAN_OP, label, region_id, before, after)
}

/// The verb of an edit that puts a region back as one side shows it
/// (`library::restore_region`). The interface writes most of them; a change
/// of a cloud result's finishing (`inference::refinish`) is written here,
/// under the lock of its manifest write, and replays like any other: the
/// side's patch revision is put back.
pub const REGION_OP: &str = "region-state";
/// The Undo tooltip of a change of a result's finishing.
pub const FINISHING_LABEL: &str = "masks.finishing.command";

/// [`record_step`] for a [`REGION_OP`] edit the backend made itself.
pub(crate) fn record_region_step(
    job: &Path,
    chapter_id: &str,
    label: &str,
    region_id: &str,
    before: Side,
    after: Side,
) -> Result<HistoryView, String> {
    push_step(job, chapter_id, REGION_OP, label, region_id, before, after)
}

fn push_step(
    job: &Path,
    chapter_id: &str,
    op: &str,
    label: &str,
    region_id: &str,
    before: Side,
    after: Side,
) -> Result<HistoryView, String> {
    push_grouped(job, chapter_id, op, label, region_id, before, after, None, None)
}

/// [`push_step`], joining the newest entry of the region's page when `group`
/// names it: a run's cleans of several regions on one page are one step of
/// that page, every region a part. The first region stays the entry's own
/// (`region_id`, `before`, `after`) as well as its first part, as the
/// interface writes an edit of several.
#[allow(clippy::too_many_arguments)]
fn push_grouped(
    job: &Path,
    chapter_id: &str,
    op: &str,
    label: &str,
    region_id: &str,
    before: Side,
    after: Side,
    group: Option<&str>,
    page: Option<usize>,
) -> Result<HistoryView, String> {
    let limit = LAST_LIMIT.load(std::sync::atomic::Ordering::Relaxed);
    let mut journal = load_for(job, chapter_id);
    // The id says the page; `page` is the caller's answer for an id that
    // does not (a fixture's).
    let page = crate::region::page_index_in(chapter_id, region_id).or(page);
    if !join_group(&mut journal, page, op, label, region_id, &before, &after, group) {
        journal.push(
            chapter_id,
            NewDelta {
                label: label.to_owned(),
                op: op.to_owned(),
                region_id: region_id.to_owned(),
                before,
                after,
                parts: Vec::new(),
                pages: Vec::new(),
                group: group.map(str::to_owned),
                page,
            },
            limit,
        );
    }
    save(job, &journal)?;
    crate::events::emit(&crate::events::Event::HistoryChanged { chapter_id: chapter_id.to_owned() });
    Ok(journal.view(limit))
}

/// Add a region to the newest entry of its page (`page`) when that entry is
/// `group`'s own clean step, of the same label, and not undone. Answers
/// whether it did.
///
/// The newest entry **of the same page**, not the journal's last: undo is
/// per page, so a step on another page that lands between two of a run's
/// cleans takes nothing back across them, and a batch over many pages is one
/// step on each. When any other step lands on the same page between two of
/// the run's cleans (a hand edit while a Nano Banana batch runs), the run's
/// next clean there starts a second step of its own. Joining the earlier
/// entry across the later one would be wrong: undo goes newest first, so the
/// later step would put back a region the batch had cleaned after it, and the
/// batch's step then a state that never followed it.
#[allow(clippy::too_many_arguments)]
fn join_group(
    journal: &mut Journal,
    page: Option<usize>,
    op: &str,
    label: &str,
    region_id: &str,
    before: &Side,
    after: &Side,
    group: Option<&str>,
) -> bool {
    let Some(group) = group else { return false };
    let Some(last) = journal.entries.iter_mut().rev().find(|entry| entry.page == page) else { return false };
    if last.undone || last.op != op || last.group.as_deref() != Some(group) || last.label != label {
        return false;
    }
    if last.parts.is_empty() {
        last.parts.push(Part { region_id: last.region_id.clone(), before: last.before.clone(), after: last.after.clone() });
    }
    match last.parts.iter_mut().find(|part| part.region_id == region_id) {
        // The same region twice in one run keeps its first `before`.
        Some(part) => part.after = after.clone(),
        None => last.parts.push(Part { region_id: region_id.to_owned(), before: before.clone(), after: after.clone() }),
    }
    true
}

/// [`record_step`] for a clean of a detected region: `before` is the region
/// as it was detected ([`side_now`], read before the commit), and the other
/// side is read here, after it. A clean of anything else (`before` is `None`)
/// records nothing. A journal that could not be written is reported on the
/// console and does not fail the clean: the result is on the page, and the
/// manifest is the record of that.
///
/// A clean made on a run's behalf (`run::on_behalf_of`, a Nano Banana batch)
/// joins that run's step on the region's page, so one Undo on a page takes
/// back the batch's cleans of that page, as long as no other step lands on
/// that page between them ([`join_group`]). A batch over 17 pages is 17
/// steps, one per page. Undo of one is refused while the run goes on
/// ([`move_at`]).
pub(crate) fn record_clean(
    job: &Path,
    chapter_id: &str,
    project: &cleaner_core::project::Project,
    source_idx: usize,
    region_id: &str,
    before: Option<Side>,
) {
    let Some(before) = before else { return };
    let Some(after) = side_now(chapter_id, project, source_idx, region_id) else { return };
    let group = crate::run::own_run();
    let label = if group.is_some() { CLEAN_BATCH_LABEL } else { CLEAN_LABEL };
    let page = project.strip.order.iter().position(|&at| at == source_idx);
    if let Err(error) = push_grouped(job, chapter_id, CLEAN_OP, label, region_id, before, after, group.as_deref(), page) {
        eprintln!("a clean of {region_id} could not be saved to undo history: {error}");
    }
}

/// The limit and its bounds, as the settings dialog shows them.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UndoLimit {
    pub value: usize,
    pub min: usize,
    pub max: usize,
    pub default: usize,
}

fn undo_limit_of(settings: &serde_json::Value) -> UndoLimit {
    UndoLimit {
        value: limit_from(settings),
        min: UNDO_LIMIT_MIN,
        max: UNDO_LIMIT_MAX,
        default: UNDO_LIMIT_DEFAULT,
    }
}

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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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
/// `op` is the vocabulary, and it has three verbs. `region-state` is the one
/// every editing surface of the interface funnels through (`maskactions`,
/// `toolapply`, `drawing`). [`CLEAN_OP`], `clean-state`, is a clean of a
/// detected region or a rejected result, and the backend writes it with the
/// commit ([`record_step`]). [`DETECTION_OP`], `detection-state`, is any
/// edit of a page's detections and held rows, and it carries `pages` instead
/// of sides ([`record_pages`]). It is a field rather than an implied constant
/// so that a verb can be added without a second file format, and so an entry
/// written by a later build is recognisably *not* something this one knows
/// how to replay.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    /// Monotonic within a chapter. Identifies an entry across a reload, which
    /// is what lets the interface hold labels alone and ask for a payload by
    /// number.
    pub seq: u64,
    /// The i18n key the Undo/Redo tooltip renders. No English crosses the seam,
    /// here or anywhere.
    pub label: String,
    /// The verb: `region-state`, `clean-state` or `detection-state`.
    pub op: String,
    /// Which region the delta is about. Empty for a `detection-state` entry,
    /// which is about pages.
    pub region_id: String,
    #[serde(default)]
    pub before: Side,
    #[serde(default)]
    pub after: Side,
    /// One edit of several regions, the first included: an eraser stroke that
    /// took paint off three Brush layers is one step. Empty for the usual
    /// edit of one region, which `region_id`, `before` and `after` say whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// A `detection-state` entry's pages: each page's state before and after,
    /// by digest ([`PageStep`]). Empty for every other verb.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<PageStep>,
    /// The run the entry was written by. A run's later edits of a page join
    /// its entry of that page rather than adding one each, so one Undo on a
    /// page takes back what the run did there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The page the entry is a step of: the 0-based page index, the number
    /// [`crate::region::page_index_in`] reads out of a region id and the
    /// interface calls the page's index. Undo and Redo on a page only ever
    /// answer that page's entries. `None` only for an entry whose page could
    /// not be told, which no page's Undo reaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    /// Whether the entry is on its page's redo side. Within one page the
    /// undone entries are always that page's newest.
    #[serde(default, skip_serializing_if = "is_false")]
    pub undone: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One region of a delta that edited several.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub region_id: String,
    pub before: Side,
    pub after: Side,
}

/// What a delta looks like on the way in, before the journal numbers it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewDelta {
    pub label: String,
    pub op: String,
    pub region_id: String,
    #[serde(default)]
    pub before: Side,
    #[serde(default)]
    pub after: Side,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub pages: Vec<PageStep>,
    #[serde(default)]
    pub group: Option<String>,
    /// The page, when the caller knows it and the entry cannot say it: a
    /// `detection-state` entry, whose page is a source index the journal
    /// cannot map. A region's page is read from its id ([`Journal::push`]).
    #[serde(default)]
    pub page: Option<usize>,
}

/// The journal as it sits on disk.
///
/// One file per chapter, partitioned by page: every entry is one page's
/// step ([`Delta::page`]), and Undo and Redo walk one page's entries only
/// ([`Journal::undo`], [`Journal::redo`]). Entries stay in the order they
/// were made, across pages, which is the order the cap trims them in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub version: u32,
    /// Zero is the legacy native-page payload; one is oriented display geometry.
    #[serde(default)]
    pub coordinate_version: u8,
    /// How many of `entries` are not undone. Kept in the file so a version 1
    /// reader still finds a number where it looks for one, and nothing steers
    /// by it: which entry Undo answers is each page's own business
    /// ([`Delta::undone`]). A version 1 file's cursor is read once, into the
    /// marks ([`load_for`]).
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
    /// Record an edit that has **already happened**. The entry's page gets
    /// it, and that page's redo side is dropped, because once a new action
    /// happens on a page its old future is no longer reachable. Every other
    /// page's redo side is left as it is.
    ///
    /// An entry whose regions are on several pages (`parts` on two pages) is
    /// kept as one entry per page, in the order the pages first appear, each
    /// with that page's parts ([`pieces`]). Answers the first.
    ///
    /// `limit` is how many entries are kept, and it is read from the setting
    /// at each push: a limit that was lowered trims the oldest entries here.
    pub fn push(&mut self, chapter_id: &str, delta: NewDelta, limit: usize) -> Delta {
        let entry = Delta {
            seq: 0,
            label: delta.label,
            op: delta.op,
            region_id: delta.region_id,
            before: delta.before,
            after: delta.after,
            parts: delta.parts,
            pages: delta.pages,
            group: delta.group,
            page: delta.page,
            undone: false,
        };
        let mut first = None;
        for mut piece in pieces(Some(chapter_id), entry) {
            piece.seq = self.next_seq;
            self.next_seq += 1;
            let page = piece.page;
            self.entries.retain(|held| held.page != page || !held.undone);
            self.entries.push(piece.clone());
            first.get_or_insert(piece);
        }
        self.trim(limit);
        self.sync_cursor();
        first.expect("an entry is at least one piece")
    }

    /// Oldest out. A page whose undone entry is trimmed loses its whole redo
    /// side with it: a redo of the next one would repeat a step on a state
    /// that never preceded it.
    fn trim(&mut self, limit: usize) {
        let excess = self.entries.len().saturating_sub(limit.max(1));
        let cut: Vec<Option<usize>> = self.entries[..excess].iter()
            .filter(|entry| entry.undone)
            .map(|entry| entry.page)
            .collect();
        self.entries.drain(..excess);
        if !cut.is_empty() {
            self.entries.retain(|entry| !(entry.undone && cut.contains(&entry.page)));
        }
    }

    /// `cursor` as the file keeps it: how many entries are not undone.
    pub(crate) fn sync_cursor(&mut self) {
        self.cursor = self.entries.iter().filter(|entry| !entry.undone).count();
    }

    /// Where the entry a step in `direction` on `page` would answer with sits:
    /// the newest entry of the page not undone, or the oldest undone one.
    fn position(&self, page: usize, direction: &str) -> Option<usize> {
        let mine = |entry: &Delta| entry.page == Some(page);
        match direction {
            "undo" => self.entries.iter().rposition(|entry| mine(entry) && !entry.undone),
            "redo" => self.entries.iter().position(|entry| mine(entry) && entry.undone),
            _ => None,
        }
    }

    /// The entry a step in `direction` on `page` would answer with, nothing
    /// marked.
    pub fn next(&self, page: usize, direction: &str) -> Option<&Delta> {
        self.position(page, direction).map(|at| &self.entries[at])
    }

    /// Mark the newest entry of `page` that is not undone as undone, and
    /// answer with it. Entries of other pages are never answered and never
    /// changed.
    pub fn undo(&mut self, page: usize) -> Option<Delta> {
        let at = self.position(page, "undo")?;
        self.entries[at].undone = true;
        self.sync_cursor();
        Some(self.entries[at].clone())
    }

    /// Clear the mark of the oldest undone entry of `page`, and answer with
    /// it.
    pub fn redo(&mut self, page: usize) -> Option<Delta> {
        let at = self.position(page, "redo")?;
        self.entries[at].undone = false;
        self.sync_cursor();
        Some(self.entries[at].clone())
    }

    /// The labels the interface holds: one short record per entry, and never
    /// a payload. This is the whole of what undo costs in RAM.
    pub fn view(&self, limit: usize) -> HistoryView {
        HistoryView {
            limit,
            cursor: self.cursor,
            entries: self
                .entries
                .iter()
                .map(|entry| HistoryLabel {
                    seq: entry.seq,
                    label: entry.label.clone(),
                    page: entry.page,
                    undone: entry.undone,
                })
                .collect(),
        }
    }
}

/// The page a region id names, when the chapter is known.
fn region_page(chapter_id: Option<&str>, region_id: &str) -> Option<usize> {
    chapter_id.and_then(|chapter_id| crate::region::page_index_in(chapter_id, region_id))
}

/// An entry as the journal keeps it: one entry per page.
///
/// A `region-state` or `clean-state` entry's page is its region's, read from
/// the id, or the page it already carries when the id does not say. One whose
/// `parts` are on several pages becomes one entry per page, in the order the
/// pages first appear, each with that page's parts, and each page's first
/// part also its own `region_id`, `before` and `after`, the convention every
/// edit of several regions keeps. A piece of one region has no `parts`, as an
/// edit of one region is written. Every piece keeps the entry's `seq`; the
/// caller numbers the others.
///
/// A `detection-state` entry is left whole: its page is the caller's to say
/// ([`record_pages`], [`migrate_v1`]).
fn pieces(chapter_id: Option<&str>, mut entry: Delta) -> Vec<Delta> {
    if entry.op == DETECTION_OP {
        return vec![entry];
    }
    if entry.parts.is_empty() {
        entry.page = region_page(chapter_id, &entry.region_id).or(entry.page);
        return vec![entry];
    }
    let fallback = region_page(chapter_id, &entry.region_id).or(entry.page);
    let mut pages: Vec<(Option<usize>, Vec<Part>)> = Vec::new();
    for part in &entry.parts {
        let page = region_page(chapter_id, &part.region_id).or(fallback);
        match pages.iter_mut().find(|(at, _)| *at == page) {
            Some((_, parts)) => parts.push(part.clone()),
            None => pages.push((page, vec![part.clone()])),
        }
    }
    if pages.len() == 1 {
        entry.page = pages[0].0;
        return vec![entry];
    }
    pages
        .into_iter()
        .map(|(page, parts)| {
            let first = &parts[0];
            Delta {
                region_id: first.region_id.clone(),
                before: first.before.clone(),
                after: first.after.clone(),
                parts: if parts.len() > 1 { parts.clone() } else { Vec::new() },
                page,
                ..entry.clone()
            }
        })
        .collect()
}

/// A version 1 journal, read into version 2: what was after the cursor is
/// marked undone, every entry is given its page, and every entry that spans
/// pages is kept as one entry per page, in place and in order. The first
/// piece keeps the entry's `seq`; the others take fresh numbers.
///
/// A `detection-state` entry's page is its first page step's source index
/// through the strip order, and the job is opened only when such an entry is
/// there. A region's page needs the chapter id, and a reader that has none
/// (`load`) leaves those entries without one.
fn migrate_v1(job: &Path, chapter_id: Option<&str>, journal: &mut Journal) {
    let cursor = journal.cursor.min(journal.entries.len());
    for entry in &mut journal.entries[cursor..] {
        entry.undone = true;
    }
    let needs_strip = journal.entries.iter().any(|entry| entry.op == DETECTION_OP && !entry.pages.is_empty());
    let strip = needs_strip
        .then(|| cleaner_core::project::Job::open(job).ok().map(|opened| opened.project.strip.order.clone()))
        .flatten();
    let page_of_source = |source_idx: usize| strip.as_ref()?.iter().position(|&at| at == source_idx);
    let mut next_seq = journal.next_seq.max(journal.entries.iter().map(|entry| entry.seq + 1).max().unwrap_or(1));
    let mut entries = Vec::with_capacity(journal.entries.len());
    for entry in std::mem::take(&mut journal.entries) {
        let mut split = if entry.op == DETECTION_OP && !entry.pages.is_empty() {
            entry.pages.iter()
                .map(|step| Delta { pages: vec![step.clone()], page: page_of_source(step.source_idx), ..entry.clone() })
                .collect()
        } else {
            pieces(chapter_id, entry)
        };
        for piece in split.iter_mut().skip(1) {
            piece.seq = next_seq;
            next_seq += 1;
        }
        entries.extend(split);
    }
    journal.entries = entries;
    journal.next_seq = next_seq;
    journal.version = JOURNAL_VERSION;
    journal.sync_cursor();
}

/// One entry, as the interface holds it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryLabel {
    pub seq: u64,
    pub label: String,
    /// The page the entry is a step of ([`Delta::page`]).
    pub page: Option<usize>,
    /// Whether it is on its page's redo side ([`Delta::undone`]).
    pub undone: bool,
}

/// The journal's index - labels, pages and marks, no payloads - and the
/// limit the journal is kept to, so the interface's index is capped by the
/// same number. `cursor` is how many entries are not undone, as the file
/// keeps it; nothing steers by it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryView {
    pub limit: usize,
    pub cursor: usize,
    pub entries: Vec<HistoryLabel>,
}

/// One step of undo or redo on one page: what to replay, and how many
/// entries of the chapter are not undone now (`cursor`).
///
/// A `detection-state` entry is replayed here, not by the interface: `pages`
/// are the page indices it wrote, for the interface to read again, and
/// `refused` is set when a page was not in the state the entry left it in
/// ([`STALE`]). The entry is marked either way.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStep {
    pub cursor: usize,
    pub entry: Option<Delta>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
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
///
/// For a reader that only looks at the entries. A version 1 file is read
/// without its regions' pages, since those need the chapter id; everything
/// that writes the journal or steers by it reads it with [`load_for`].
pub fn load(job: &Path) -> Journal {
    read(job, None)
}

/// [`load`], with the chapter the job is: a version 1 file is read whole into
/// version 2, every entry with its page ([`migrate_v1`]).
pub fn load_for(job: &Path, chapter_id: &str) -> Journal {
    read(job, Some(chapter_id))
}

fn read(job: &Path, chapter_id: Option<&str>) -> Journal {
    let path = journal_path(job);
    let Ok(bytes) = std::fs::read(&path) else {
        return Journal::default();
    };
    let mut journal = serde_json::from_slice::<Journal>(&bytes).unwrap_or_default();
    let legacy = journal.version < 2;
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
    if legacy {
        migrate_v1(job, chapter_id, &mut journal);
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
    let mut journal = load_for(&job, chapter_id);
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
        let journal = load_for(&job, &chapter_id);
        // Opening a chapter is when page states no entry names any more go:
        // under the job's lock, so a capture in flight is never swept, and
        // skipped when another writer has the chapter.
        if let Ok(_lock) = crate::run::lock_job(&job) {
            sweep_blobs(&sidecar_dir(&job), &journal);
        }
        Ok(journal.view(limit_for(&app)))
    })
    .await
}

/// How many undo steps a chapter keeps, and the bounds a new value must be in.
#[tauri::command]
pub fn undo_limit(app: tauri::AppHandle) -> UndoLimit {
    let limit = undo_limit_of(&crate::settings::read(&app).unwrap_or(serde_json::Value::Null));
    LAST_LIMIT.store(limit.value, std::sync::atomic::Ordering::Relaxed);
    limit
}

#[tauri::command]
pub async fn history_push(
    app: tauri::AppHandle,
    chapter_id: String,
    entry: NewDelta,
) -> Result<HistoryView, String> {
    crate::library::blocking(move || {
        let limit = limit_for(&app);
        with_journal(&app, &chapter_id, |journal| {
            journal.push(&chapter_id, entry, limit);
            journal.view(limit)
        })
    })
    .await
}

/// One step on one page, in the direction named: `page_index` is the page on
/// screen, and only its entries are ever answered or marked. Undo and redo
/// never leave that page. The mark moves **on disk** before the interface
/// replays anything, which is what makes an undo interrupted by a crash
/// resolve the same way twice: the manifest edit is the thing that may or may
/// not have happened, and re-applying a delta to a manifest already in that
/// state is a no-op by construction (`set_mask_visible` answers "it is as you
/// asked" rather than refusing).
///
/// **Refused before anything is marked** when the entry it would replay is of
/// a page a run is working on (`page_busy`). The replay is an edit of that
/// page and would be refused itself ([`crate::run::lock_page`]), and a mark
/// that had moved on would say an edit was taken back that was not.
///
/// **Refused the same way when the entry is a run's that is still going**
/// (`run_step_in_progress`). The run writes each page it finishes into its
/// step of that page. Taken back mid-run, the step would hold only what was
/// done so far, and the run's later work there would make a second step that
/// one Undo no longer takes back.
#[tauri::command]
pub async fn history_move(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: usize,
    direction: String,
) -> Result<HistoryStep, String> {
    crate::library::blocking(move || {
        let job = crate::library::resolve_chapter(&app, &chapter_id)?;
        move_at(&job, &chapter_id, page_index, &direction)
    })
    .await
}

/// What [`move_at`] answers for a step whose run is still going. Starts with
/// its code, which the interface reads.
const RUN_STEP_IN_PROGRESS: &str = "run_step_in_progress: the step belongs to a run that is still going";

/// The command's body, on a chapter already resolved to its job.
pub(crate) fn move_at(job: &Path, chapter_id: &str, page_index: usize, direction: &str) -> Result<HistoryStep, String> {
    let _lock = crate::run::lock_job(job)?;
    let mut journal = load_for(job, chapter_id);
    let next = journal.next(page_index, direction).cloned();
    if next.as_ref().and_then(|entry| entry.group.as_deref()).is_some_and(crate::run::is_running) {
        return Err(RUN_STEP_IN_PROGRESS.to_owned());
    }
    if let Some(page_index) = next.as_ref().and_then(|entry| entry.page) {
        if crate::run::page_run(job, chapter_id, page_index).is_some() {
            return Err(crate::run::WriteRefused::PageBusy { page_index }.into());
        }
    }
    // A detection edit is put back here, under the lock that moves the
    // mark, so no run can reach its pages between the two.
    let (pages, refused) = match next.as_ref().filter(|entry| entry.op == DETECTION_OP) {
        Some(entry) => replay_pages(job, chapter_id, entry, direction == "undo")?,
        None => (Vec::new(), None),
    };
    let entry = match direction {
        "undo" => journal.undo(page_index),
        "redo" => journal.redo(page_index),
        _ => None,
    };
    save(job, &journal)?;
    Ok(HistoryStep {
        cursor: journal.cursor,
        entry,
        pages,
        refused,
    })
}

/* ------------------------------------------------------------------ */
/* Page states: what an edit of a page's detections is undone to       */
/* ------------------------------------------------------------------ */

/// The verb of an edit of a page's detections and held rows: a detection
/// deleted, its mask brushed or padded, its text type changed, a held row
/// approved or dismissed, a page detected. A detection has no pixels and
/// no `visible` flag, so these could not be undone by turning a record off,
/// the way a patch's are.
///
/// So the entry names **whole page states**, before and after, by digest. A
/// page state is the page's detection records, its held rows, and the digest
/// of every sidecar file they name, written once into `history/` beside this
/// journal ([`capture_page`]). Every file there is named by its own SHA-256:
/// an unchanged mask is the same file in every state that holds it, and the
/// state after one edit is the same file as the state before the next. What
/// an edit costs on disk is what it changed. Nothing is held in memory.
///
/// The backend replays the entry itself, in [`move_at`], under the lock that
/// moves the cursor ([`replay_pages`]).
pub const DETECTION_OP: &str = "detection-state";
/// The directory, inside `<job>.mtclean.d/`, that page states and the files
/// they name are kept in.
pub const BLOB_DIR: &str = "history";
/// The code a replay answers with when a page is not in the state the entry
/// left it in. Something this journal does not know about changed the page,
/// and putting the entry's state back would undo that too, so nothing is
/// written. The cursor still moves, so the steps below stay reachable.
pub const STALE: &str = "history_stale";

/// One page of a `detection-state` entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PageStep {
    pub source_idx: usize,
    /// The digest of the page state before the edit.
    pub before: String,
    /// The digest of the page state after it.
    pub after: String,
    /// Whether the page was on `Project::redetect` before the edit and after
    /// it. Kept beside the digests and not in the page state, so a page put on
    /// the list by an edit this journal does not record (a whole mask hidden)
    /// does not make every step of the page stale. `None` in a step written
    /// before the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_before: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_after: Option<bool>,
}

/// A page's detections and held rows, as a blob in `history/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageState {
    detections: Vec<cleaner_core::project::DetectedRegion>,
    untouched: Vec<cleaner_core::project::RegionUntouched>,
    /// Every sidecar file the records name, by its path relative to the
    /// sidecar, and the digest of its bytes. A file that was not on disk is
    /// left out, and a restore leaves it out too.
    files: std::collections::BTreeMap<String, String>,
}

fn blob_dir(sidecar: &Path) -> PathBuf {
    sidecar.join(BLOB_DIR)
}

fn is_digest(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A path a page state may name: relative, inside the sidecar, in one of the
/// directories detections and held rows keep their files in.
fn is_page_file(reference: &str) -> bool {
    let path = Path::new(reference);
    ["detections/", "candidates/", "evidence/"].iter().any(|dir| reference.starts_with(dir))
        && path.components().all(|part| matches!(part, std::path::Component::Normal(_)))
}

fn digest_of(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// Keep `bytes` in `history/` under their digest, once.
fn put_blob(sidecar: &Path, bytes: &[u8]) -> Result<String, String> {
    let digest = digest_of(bytes);
    let path = blob_dir(sidecar).join(&digest);
    if !path.exists() {
        std::fs::create_dir_all(blob_dir(sidecar)).map_err(|e| e.to_string())?;
        buffers::write_atomic(&path, bytes).map_err(|e| e.to_string())?;
    }
    Ok(digest)
}

/// A blob's bytes, checked against the digest that names them.
fn get_blob(sidecar: &Path, digest: &str) -> Result<Vec<u8>, String> {
    if !is_digest(digest) {
        return Err(format!("{STALE}: {digest} is not a page state"));
    }
    let path = blob_dir(sidecar).join(digest);
    let bytes = std::fs::read(&path).map_err(|e| format!("{STALE}: {}: {e}", path.display()))?;
    if digest_of(&bytes) != digest {
        return Err(format!("{STALE}: {} fails its digest", path.display()));
    }
    Ok(bytes)
}

fn read_state(sidecar: &Path, digest: &str) -> Result<PageState, String> {
    serde_json::from_slice(&get_blob(sidecar, digest)?).map_err(|e| format!("{STALE}: {e}"))
}

/// The files a page state names, each with the digest its record already
/// carries, if it carries one: a mask whose digest is known and whose blob is
/// kept already is not read again.
fn page_files(state: &PageState) -> Vec<(String, Option<String>)> {
    let mut files = Vec::new();
    for record in &state.detections {
        for reference in record.files() {
            let known = (reference == record.mask_ref && !record.mask_sha256.is_empty())
                .then(|| record.mask_sha256.clone());
            files.push((reference, known));
        }
        if let Some(group) = &record.group {
            if let Some(reference) = &group.evidence_ref {
                files.push((reference.clone(), group.evidence_sha256.clone()));
            }
        }
    }
    for row in &state.untouched {
        if let Some(held) = &row.lettering {
            let known = held.mask_ref.strip_prefix("candidates/")
                .and_then(|name| name.strip_suffix(".mask"))
                .filter(|name| is_digest(name))
                .map(str::to_owned);
            files.push((held.mask_ref.clone(), known));
        }
    }
    files
}

/// Write the state of one page into `history/` and answer its digest. The
/// same page in the same state is the same digest, so a caller can tell
/// whether an edit changed anything by comparing two.
pub(crate) fn capture_page(job: &cleaner_core::project::Job, source_idx: usize) -> Result<String, String> {
    let sidecar = job.sidecar();
    let mut state = PageState {
        detections: job.project.detections.iter().filter(|row| row.source_idx == source_idx).cloned().collect(),
        untouched: job.project.regions_untouched.iter().filter(|row| row.source_idx == source_idx).cloned().collect(),
        files: Default::default(),
    };
    for (reference, known) in page_files(&state) {
        if !is_page_file(&reference) || state.files.contains_key(&reference) {
            continue;
        }
        if let Some(digest) = known.filter(|digest| is_digest(digest) && blob_dir(sidecar).join(digest).exists()) {
            state.files.insert(reference, digest);
            continue;
        }
        match std::fs::read(sidecar.join(&reference)) {
            Ok(bytes) => {
                let digest = put_blob(sidecar, &bytes)?;
                state.files.insert(reference, digest);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{reference}: {error}")),
        }
    }
    put_blob(sidecar, &serde_json::to_vec(&state).map_err(|e| e.to_string())?)
}

/// [`capture_page`] for every page named, to pass to [`record_pages`] once
/// the edit is written. A page that cannot be captured is left out, and its
/// edit is not undoable.
pub(crate) fn capture_pages(job: &cleaner_core::project::Job, sources: &[usize]) -> Vec<(usize, String, bool)> {
    sources.iter().filter_map(|&source_idx| match capture_page(job, source_idx) {
        Ok(digest) => Some((source_idx, digest, job.project.redetect.contains(&source_idx))),
        Err(error) => {
            eprintln!("page {source_idx} could not be saved to undo history: {error}");
            None
        }
    }).collect()
}

/// Record an edit of these pages' detections as one undo step **per page**,
/// from the states [`capture_pages`] took before it: undo is per page, so an
/// edit that changed three pages is a step on each, and each page's Undo
/// takes back its own part. A page the edit left as it was is not part of the
/// step, and an edit that changed no page records nothing.
///
/// With a `group` a page's step joins the newest entry of that page when that
/// entry is the same group's, of the same label, and not undone: a Detect run
/// over 17 pages is 17 steps, one per page, and a page it writes twice keeps
/// its first `before`. A page the run's writes brought back to where it
/// started has no step left, and its entry goes.
///
/// **The caller holds the job's lock**, as for [`record_step`]. A journal
/// that cannot be written is said on the console and does not fail the edit:
/// the manifest is the record of what was done.
pub(crate) fn record_pages(
    job: &cleaner_core::project::Job,
    chapter_id: &str,
    label: &str,
    group: Option<&str>,
    before: Vec<(usize, String, bool)>,
) {
    let mut pages = Vec::new();
    for (source_idx, before, queued_before) in before {
        match capture_page(job, source_idx) {
            Ok(after) if after != before => pages.push(PageStep {
                source_idx, before, after,
                queued_before: Some(queued_before),
                queued_after: Some(job.project.redetect.contains(&source_idx)),
            }),
            Ok(_) => {}
            Err(error) => {
                eprintln!("an edit of page {source_idx} could not be saved to undo history: {error}");
                return;
            }
        }
    }
    if pages.is_empty() {
        return;
    }
    let path = job.path();
    let limit = LAST_LIMIT.load(std::sync::atomic::Ordering::Relaxed);
    let mut journal = load_for(path, chapter_id);
    for step in pages {
        let page = job.project.strip.order.iter().position(|&at| at == step.source_idx);
        let joined = group.and_then(|group| {
            let at = journal.entries.iter().rposition(|entry| entry.page == page)?;
            let last = &journal.entries[at];
            (!last.undone && last.op == DETECTION_OP && last.group.as_deref() == Some(group) && last.label == label)
                .then_some(at)
        });
        match joined {
            Some(at) => {
                let last = &mut journal.entries[at];
                match last.pages.iter_mut().find(|held| held.source_idx == step.source_idx) {
                    Some(held) => {
                        held.after = step.after;
                        held.queued_after = step.queued_after;
                    }
                    None => last.pages.push(step),
                }
                last.pages.retain(|page| page.before != page.after);
                if last.pages.is_empty() {
                    journal.entries.remove(at);
                    journal.sync_cursor();
                }
            }
            None => {
                journal.push(
                    chapter_id,
                    NewDelta {
                        label: label.to_owned(),
                        op: DETECTION_OP.to_owned(),
                        region_id: String::new(),
                        before: Side::default(),
                        after: Side::default(),
                        parts: Vec::new(),
                        pages: vec![step],
                        group: group.map(str::to_owned),
                        page,
                    },
                    limit,
                );
            }
        }
    }
    if let Err(error) = save(path, &journal) {
        eprintln!("an edit of {chapter_id} could not be saved to undo history: {error}");
        return;
    }
    crate::events::emit(&crate::events::Event::HistoryChanged { chapter_id: chapter_id.to_owned() });
}

/// Put one `detection-state` entry's pages back, in the direction asked for,
/// in one manifest write. Answers the page indices written, and [`STALE`]
/// when a page was not in the state the entry left it in, in which case
/// nothing at all is written. A page already in the state asked for is not
/// written again, so a replay may be asked twice.
///
/// Refused with `page_busy` before anything is read when a run is on one of
/// the pages, as every edit of a page is.
fn replay_pages(
    job_path: &Path,
    chapter_id: &str,
    entry: &Delta,
    undo: bool,
) -> Result<(Vec<usize>, Option<String>), String> {
    let mut job = cleaner_core::project::Job::open(job_path).map_err(|e| e.to_string())?;
    let page_of = |job: &cleaner_core::project::Job, source_idx: usize| {
        job.project.strip.order.iter().position(|&at| at == source_idx)
    };
    for step in &entry.pages {
        if let Some(page_index) = page_of(&job, step.source_idx) {
            if crate::run::page_run(job_path, chapter_id, page_index).is_some() {
                return Err(crate::run::WriteRefused::PageBusy { page_index }.into());
            }
        }
    }
    let sidecar = job.sidecar().to_path_buf();
    // Every page is checked, and every target read, before anything is written.
    let mut targets = Vec::new();
    for step in &entry.pages {
        let (from, to) = if undo { (&step.after, &step.before) } else { (&step.before, &step.after) };
        let queued = if undo { step.queued_before } else { step.queued_after };
        let now = capture_page(&job, step.source_idx)?;
        if now == *to {
            continue;
        }
        if now != *from {
            return Ok((Vec::new(), Some(STALE.to_owned())));
        }
        match read_state(&sidecar, to) {
            Ok(state) => targets.push((step.source_idx, state, queued)),
            Err(error) => {
                eprintln!("an undo step of {chapter_id} could not be read: {error}");
                return Ok((Vec::new(), Some(STALE.to_owned())));
            }
        }
    }
    if targets.is_empty() {
        let pages = entry.pages.iter().filter_map(|step| page_of(&job, step.source_idx)).collect();
        return Ok((pages, None));
    }
    // A file the manifest names now is never written over: a state that
    // would change one is not the state this page came from.
    let named: std::collections::HashSet<String> = job.project.detections.iter()
        .chain(&job.project.cleaned_detections)
        .flat_map(|row| row.files())
        .chain(job.project.regions_untouched.iter().filter_map(|row| Some(row.lettering.as_ref()?.mask_ref.clone())))
        .collect();
    let mut writes = Vec::new();
    for (source_idx, state, _) in &targets {
        let clash = state.detections.iter().any(|row| {
            row.source_idx != *source_idx
                || job.project.patches.iter().any(|patch| patch.id == row.id)
                || job.project.detections.iter().any(|held| held.id == row.id && held.source_idx != *source_idx)
        }) || state.untouched.iter().any(|row| row.source_idx != *source_idx);
        if clash {
            return Ok((Vec::new(), Some(STALE.to_owned())));
        }
        for (reference, digest) in &state.files {
            if !is_page_file(reference) {
                return Ok((Vec::new(), Some(STALE.to_owned())));
            }
            let path = sidecar.join(reference);
            match std::fs::read(&path) {
                Ok(bytes) if digest_of(&bytes) == *digest => continue,
                Ok(_) if named.contains(reference) => return Ok((Vec::new(), Some(STALE.to_owned()))),
                _ => {}
            }
            match get_blob(&sidecar, digest) {
                Ok(bytes) => writes.push((path, bytes)),
                Err(error) => {
                    eprintln!("an undo step of {chapter_id} could not be read: {error}");
                    return Ok((Vec::new(), Some(STALE.to_owned())));
                }
            }
        }
    }
    for (path, bytes) in &writes {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        buffers::write_atomic(path, bytes).map_err(|e| e.to_string())?;
    }
    let mut pages = Vec::new();
    for (source_idx, state, queued) in targets {
        // The page goes back on `Project::redetect` as the step found it, and
        // on it whenever the step takes away a detection the page holds: the
        // undo of a Detect leaves its pages recorded detected with nothing
        // on them, and a step from before the record says nothing. A step
        // that takes away a row the user made (the undo of a drawn region)
        // queues a Detect that finds nothing new: work, never a loss.
        let drops_one = job.project.detections.iter()
            .any(|row| row.source_idx == source_idx && !state.detections.iter().any(|kept| kept.id == row.id));
        if drops_one || queued == Some(true) {
            job.queue_redetect(source_idx);
        } else if queued == Some(false) {
            job.project.redetect.retain(|index| *index != source_idx);
        }
        job.project.detections.retain(|row| row.source_idx != source_idx);
        job.project.detections.extend(state.detections);
        job.project.regions_untouched.retain(|row| row.source_idx != source_idx);
        job.project.regions_untouched.extend(state.untouched);
        pages.extend(page_of(&job, source_idx));
    }
    job.flush().map_err(|e| e.to_string())?;
    crate::library::invalidate_manifest_cache(job_path);
    Ok((pages, None))
}

/// Delete the page states and files in `history/` that no entry names any
/// more: the entries a push dropped, trimmed or truncated. Run when a chapter
/// is opened, under its lock. A state that cannot be read keeps nothing.
pub(crate) fn sweep_blobs(sidecar: &Path, journal: &Journal) {
    let Ok(listing) = std::fs::read_dir(blob_dir(sidecar)) else {
        return;
    };
    let mut keep = std::collections::HashSet::new();
    for step in journal.entries.iter().flat_map(|entry| &entry.pages) {
        for digest in [&step.before, &step.after] {
            if keep.insert(digest.clone()) {
                if let Ok(state) = read_state(sidecar, digest) {
                    keep.extend(state.files.into_values());
                }
            }
        }
    }
    for item in listing.flatten() {
        let name = item.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_digest(name) && !keep.contains(name) {
            let _ = std::fs::remove_file(item.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAPTER: &str = "chapter";

    /// A region id on `page` (0-based), built as every region id in this
    /// application is: on its page's id.
    fn on(page: usize, n: usize) -> String {
        format!("{CHAPTER}-p{:03}-r{n}", page + 1)
    }

    fn delta(label: &str, region: &str) -> NewDelta {
        NewDelta {
            parts: Vec::new(),
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
            }, ..Default::default() }
    }

    fn push(journal: &mut Journal, label: &str, region: &str) -> Delta {
        journal.push(CHAPTER, delta(label, region), UNDO_LIMIT_DEFAULT)
    }

    fn labels(journal: &Journal) -> Vec<String> {
        journal.entries.iter().map(|entry| entry.label.clone()).collect()
    }

    #[test]
    fn an_entry_from_the_interface_needs_no_pages_parts_or_group() {
        // What `regionEntry` sends for a plain edit of one region.
        let delta: NewDelta = serde_json::from_value(serde_json::json!({
            "label": "masks.command.rerunMask", "op": "region-state", "regionId": "r1",
            "before": {"present": false}, "after": {"present": true},
        })).unwrap();
        assert!(delta.parts.is_empty() && delta.pages.is_empty() && delta.group.is_none() && delta.page.is_none());
    }

    /// A Nano Banana batch's cleans of one page join one step: every region a
    /// part, the first included, a region cleaned twice keeping its first
    /// `before`. Another run's clean, an ordinary clean, or anything after an
    /// undo starts its own entry.
    #[test]
    fn a_runs_cleans_join_its_one_step() {
        let side = |status: &str| Side { present: true, page_status: Some(status.into()), region: None };
        let grouped = |journal: &mut Journal, region: &str, before: &str, after: &str, group: Option<&str>| {
            let page = crate::region::page_index_in(CHAPTER, region);
            if !join_group(journal, page, CLEAN_OP, CLEAN_BATCH_LABEL, region, &side(before), &side(after), group) {
                journal.push(CHAPTER, NewDelta {
                    label: CLEAN_BATCH_LABEL.into(), op: CLEAN_OP.into(), region_id: region.into(),
                    before: side(before), after: side(after), group: group.map(str::to_owned), ..Default::default()
                }, UNDO_LIMIT_DEFAULT);
            }
        };
        let mut journal = Journal::default();
        grouped(&mut journal, &on(0, 1), "detected", "a", Some("run-1"));
        grouped(&mut journal, &on(0, 2), "detected", "b", Some("run-1"));
        grouped(&mut journal, &on(0, 1), "a", "c", Some("run-1"));
        assert_eq!(journal.entries.len(), 1);
        let parts: Vec<_> = journal.entries[0].parts.iter()
            .map(|part| (part.region_id.clone(), part.before.page_status.as_deref(), part.after.page_status.as_deref()))
            .collect();
        assert_eq!(parts, [(on(0, 1), Some("detected"), Some("c")), (on(0, 2), Some("detected"), Some("b"))]);

        grouped(&mut journal, &on(0, 3), "detected", "d", Some("run-2"));
        assert_eq!(journal.entries.len(), 2, "another run's clean is its own step");
        assert!(!join_group(&mut journal, Some(0), CLEAN_OP, CLEAN_BATCH_LABEL, &on(0, 4), &side("x"), &side("y"), None));
        journal.undo(0);
        assert!(!join_group(&mut journal, Some(0), CLEAN_OP, CLEAN_BATCH_LABEL, &on(0, 4), &side("x"), &side("y"), Some("run-2")),
            "nothing joins a step that was undone");
    }

    /// The split `join_group` documents: a step that lands on the same page
    /// between two of a batch's cleans ends the batch's step there, and the
    /// next clean starts another, so undo never runs across the later step. A
    /// step on another page splits nothing.
    #[test]
    fn a_step_between_two_of_a_runs_cleans_splits_its_step() {
        let side = |status: &str| Side { present: true, page_status: Some(status.into()), region: None };
        let mut journal = Journal::default();
        journal.push(CHAPTER, NewDelta {
            label: CLEAN_BATCH_LABEL.into(), op: CLEAN_OP.into(), region_id: on(0, 1),
            before: side("detected"), after: side("a"), group: Some("run-1".into()), ..Default::default()
        }, UNDO_LIMIT_DEFAULT);
        push(&mut journal, "masks.command.rerunMask", &on(1, 9));
        assert!(join_group(&mut journal, Some(0), CLEAN_OP, CLEAN_BATCH_LABEL, &on(0, 2), &side("detected"), &side("b"), Some("run-1")),
            "a step on another page is not between them");
        push(&mut journal, "masks.command.rerunMask", &on(0, 9));
        assert!(!join_group(&mut journal, Some(0), CLEAN_OP, CLEAN_BATCH_LABEL, &on(0, 3), &side("detected"), &side("c"), Some("run-1")));
        assert_eq!(journal.entries[0].parts.len(), 2, "the earlier step is left as it was");
    }

    /// A run's cleans over two pages are a step on each, and each page's Undo
    /// takes back its own part and nothing of the other's.
    #[test]
    fn a_runs_group_over_two_pages_is_two_steps() {
        let side = |status: &str| Side { present: true, page_status: Some(status.into()), region: None };
        let mut journal = Journal::default();
        for (page, n) in [(0, 1), (1, 2), (0, 3), (1, 4)] {
            let region = on(page, n);
            if !join_group(&mut journal, Some(page), CLEAN_OP, CLEAN_BATCH_LABEL, &region, &side("detected"), &side("a"), Some("run-1")) {
                journal.push(CHAPTER, NewDelta {
                    label: CLEAN_BATCH_LABEL.into(), op: CLEAN_OP.into(), region_id: region,
                    before: side("detected"), after: side("a"), group: Some("run-1".into()), ..Default::default()
                }, UNDO_LIMIT_DEFAULT);
            }
        }
        assert_eq!(journal.entries.iter().map(|entry| (entry.page, entry.parts.len())).collect::<Vec<_>>(),
            [(Some(0), 2), (Some(1), 2)]);
        let undone = journal.undo(1).unwrap();
        assert_eq!(undone.parts.iter().map(|part| part.region_id.clone()).collect::<Vec<_>>(), [on(1, 2), on(1, 4)]);
        assert!(journal.undo(1).is_none());
        assert!(!journal.entries[0].undone);
        assert_eq!(journal.undo(0).unwrap().region_id, on(0, 1));
    }

    #[test]
    fn a_push_numbers_the_entry_and_moves_the_cursor_to_the_end() {
        let mut journal = Journal::default();
        let first = push(&mut journal, "masks.command.deleteMask", &on(0, 1));
        let second = push(&mut journal, "masks.command.rerunMask", &on(0, 2));
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
        assert_eq!(journal.cursor, 2);
        assert_eq!(journal.view(UNDO_LIMIT_DEFAULT).entries.len(), 2);
    }

    /// The interface's copy is labels, pages and marks, and nothing else -
    /// the payload never leaves this file until it is replayed.
    #[test]
    fn the_view_carries_no_payload() {
        let mut journal = Journal::default();
        push(&mut journal, "masks.command.deleteMask", &on(3, 1));
        let view = journal.view(UNDO_LIMIT_DEFAULT);
        assert_eq!(view.limit, UNDO_LIMIT_DEFAULT);
        assert_eq!(
            view.entries[0],
            HistoryLabel {
                seq: 1,
                label: "masks.command.deleteMask".into(),
                page: Some(3),
                undone: false,
            }
        );
    }

    #[test]
    fn undo_and_redo_walk_the_same_entries() {
        let mut journal = Journal::default();
        push(&mut journal, "a", &on(0, 1));
        push(&mut journal, "b", &on(0, 2));

        assert_eq!(journal.undo(0).unwrap().label, "b");
        assert_eq!(journal.cursor, 1);
        assert_eq!(journal.undo(0).unwrap().label, "a");
        assert_eq!(journal.cursor, 0);
        assert!(
            journal.undo(0).is_none(),
            "the bottom of the past is not a step"
        );

        assert_eq!(journal.redo(0).unwrap().label, "a");
        assert_eq!(journal.redo(0).unwrap().label, "b");
        assert!(journal.redo(0).is_none());
    }

    /// The rule the in-memory history had and the file has to keep, on one
    /// page: once a new action happens the old future is unreachable.
    #[test]
    fn a_push_after_an_undo_drops_the_future() {
        let mut journal = Journal::default();
        push(&mut journal, "a", &on(0, 1));
        push(&mut journal, "b", &on(0, 2));
        journal.undo(0);
        push(&mut journal, "c", &on(0, 3));

        let labels: Vec<_> = journal
            .view(UNDO_LIMIT_DEFAULT)
            .entries
            .iter()
            .map(|e| e.label.clone())
            .collect();
        assert_eq!(labels, vec!["a", "c"]);
        assert_eq!(journal.cursor, 2);
        assert!(journal.redo(0).is_none());
    }

    /// Undo on page B, pressed many times, takes back B's steps newest first
    /// and stops when B has none left. Not one of page A's entries is
    /// answered or changed, though they are interleaved with B's.
    #[test]
    fn undo_on_one_page_never_answers_or_changes_another_pages_entries() {
        let mut journal = Journal::default();
        for n in 0..3 {
            push(&mut journal, &format!("a{n}"), &on(0, n));
            push(&mut journal, &format!("b{n}"), &on(1, n));
        }
        let page_a = |journal: &Journal| journal.entries.iter().filter(|entry| entry.page == Some(0)).cloned().collect::<Vec<_>>();
        let a_before = page_a(&journal);
        let mut undone = Vec::new();
        for _ in 0..10 {
            if let Some(entry) = journal.undo(1) {
                assert_eq!(entry.page, Some(1));
                undone.push(entry.label);
            }
        }
        assert_eq!(undone, ["b2", "b1", "b0"]);
        assert!(journal.next(1, "undo").is_none());
        assert_eq!(page_a(&journal), a_before, "page A was not touched");
        assert_eq!(journal.next(0, "undo").unwrap().label, "a2");
        assert_eq!(journal.cursor, 3);
    }

    /// A push on page B after an undo on B drops B's redo side only; page A's
    /// undone steps stay where they are, and Redo on A still repeats them.
    #[test]
    fn a_push_on_one_page_keeps_another_pages_redo_side() {
        let mut journal = Journal::default();
        push(&mut journal, "a0", &on(0, 0));
        push(&mut journal, "b0", &on(1, 0));
        push(&mut journal, "b1", &on(1, 1));
        journal.undo(0);
        journal.undo(1);
        push(&mut journal, "b2", &on(1, 2));
        assert_eq!(labels(&journal), ["a0", "b0", "b2"]);
        assert!(journal.entries[0].undone, "A's undone step is still on A's redo side");
        assert!(journal.redo(1).is_none(), "B's redo side went with the push");
        assert_eq!(journal.redo(0).unwrap().label, "a0");
    }

    /// Redo on page A still works after edits made on page B since.
    #[test]
    fn redo_on_a_page_works_after_edits_on_another() {
        let mut journal = Journal::default();
        push(&mut journal, "a0", &on(0, 0));
        push(&mut journal, "a1", &on(0, 1));
        journal.undo(0);
        journal.undo(0);
        push(&mut journal, "b0", &on(1, 0));
        push(&mut journal, "b1", &on(1, 1));
        assert_eq!(journal.redo(0).unwrap().label, "a0");
        assert_eq!(journal.redo(0).unwrap().label, "a1");
        assert!(journal.redo(0).is_none());
        assert_eq!(journal.undo(1).unwrap().label, "b1");
    }

    /// An entry pushed with parts on two pages is kept as one entry per page,
    /// each page's first part also the entry's own region, and numbered in
    /// order.
    #[test]
    fn a_pushed_entry_over_two_pages_is_a_step_on_each() {
        let part = |region: String| Part { region_id: region, before: Side::default(), after: Side { present: true, ..Default::default() } };
        let mut journal = Journal::default();
        let first = journal.push(CHAPTER, NewDelta {
            label: "masks.command.deleteMask".into(), op: REGION_OP.into(), region_id: on(0, 1),
            parts: vec![part(on(0, 1)), part(on(1, 2)), part(on(0, 3))], ..Default::default()
        }, UNDO_LIMIT_DEFAULT);
        assert_eq!(first.seq, 1);
        let shape: Vec<_> = journal.entries.iter()
            .map(|entry| (entry.seq, entry.page, entry.region_id.clone(),
                entry.parts.iter().map(|part| part.region_id.clone()).collect::<Vec<_>>()))
            .collect();
        assert_eq!(shape, [
            (1, Some(0), on(0, 1), vec![on(0, 1), on(0, 3)]),
            (2, Some(1), on(1, 2), vec![]),
        ]);
        assert!(journal.entries[1].after.present, "a piece of one region keeps its sides");
    }

    /// Oldest out, and no number reissued - a `seq` that came back would make
    /// two different edits the same entry to any caller holding the old one.
    #[test]
    fn the_journal_is_capped_and_never_reissues_a_number() {
        for limit in [UNDO_LIMIT_MIN, UNDO_LIMIT_DEFAULT, UNDO_LIMIT_MAX] {
            let mut journal = Journal::default();
            for n in 0..limit + 10 {
                journal.push(CHAPTER, delta(&format!("e{n}"), &on(n % 3, n)), limit);
            }
            assert_eq!(journal.entries.len(), limit);
            assert_eq!(journal.cursor, limit);
            assert_eq!(journal.entries[0].label, "e10");
            assert_eq!(journal.next_seq, limit as u64 + 11);
        }
    }

    /// A page whose undone step is trimmed loses its whole redo side: the
    /// next one would be repeated on a state that never preceded it.
    #[test]
    fn trimming_an_undone_entry_drops_its_pages_redo_side() {
        let mut journal = Journal::default();
        push(&mut journal, "a0", &on(0, 0));
        push(&mut journal, "a1", &on(0, 1));
        journal.undo(0);
        journal.undo(0);
        for n in 0..UNDO_LIMIT_MIN - 1 {
            journal.push(CHAPTER, delta(&format!("b{n}"), &on(1, n)), UNDO_LIMIT_MIN);
        }
        assert!(journal.entries.iter().all(|entry| entry.page == Some(1)));
        assert!(journal.redo(0).is_none());
    }

    /// The setting is read at each push, so a lower number takes effect at the
    /// next edit: the oldest entries go, the newest stay, and Undo still
    /// answers with the edit that was just made.
    #[test]
    fn lowering_the_limit_trims_the_oldest_at_the_next_push() {
        let mut journal = Journal::default();
        for n in 0..100 {
            push(&mut journal, &format!("e{n}"), &on(0, n));
        }
        // Two steps back, so the push also has a future to drop.
        journal.undo(0);
        journal.undo(0);
        journal.push(CHAPTER, delta("new", &on(0, 1)), UNDO_LIMIT_MIN);

        assert_eq!(journal.entries.len(), UNDO_LIMIT_MIN);
        assert_eq!(journal.cursor, UNDO_LIMIT_MIN);
        assert_eq!(journal.entries[0].label, "e79");
        assert_eq!(journal.undo(0).unwrap().label, "new");
        assert_eq!(journal.undo(0).unwrap().label, "e97");
        assert!(journal.redo(0).is_some());
    }

    /// A job of two pages read in the other order: page 0 is source 1. For
    /// the tests that need a strip to read a source index through.
    fn two_page_job(name: &str) -> (PathBuf, cleaner_core::project::Job) {
        use cleaner_core::image::{encode, Format};
        use cleaner_core::project::{Job, Project, StripMode};
        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        let root = std::env::temp_dir().join(format!("mtclean-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raws")).unwrap();
        let bytes = encode(&raw, Format::Png).unwrap();
        let sources: Vec<_> = ["a", "b"].iter().map(|stem| {
            let path = root.join(format!("raws/{stem}.png"));
            std::fs::write(&path, &bytes).unwrap();
            cleaner_core::ingest::source_ref(&path, &bytes).unwrap()
        }).collect();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let mut job = Job::create(&manifest, Project::new(manifest.parent().unwrap(), "test", StripMode::Single, &sources)).unwrap();
        job.project.strip.order = vec![1, 0];
        job.flush().unwrap();
        (root, job)
    }

    /// A version 1 journal, cursor in the middle and a run's entry over two
    /// pages, reads into version 2: what was after the cursor is undone, every
    /// entry has its page, and each entry over two pages is one per page, in
    /// place, the first keeping its number and the others taking new ones.
    #[test]
    fn a_version_1_journal_loads_into_marks_and_one_entry_per_page() {
        let (root, job) = two_page_job("v1");
        let side = |present: bool| serde_json::json!({ "present": present });
        let page_step = |source: usize| serde_json::json!({ "sourceIdx": source, "before": "x", "after": "y" });
        let v1 = serde_json::json!({
            "version": 1, "coordinateVersion": 1, "cursor": 2, "nextSeq": 5,
            "entries": [
                { "seq": 1, "label": "e1", "op": REGION_OP, "regionId": on(0, 1), "before": side(true), "after": side(false) },
                { "seq": 2, "label": CLEAN_BATCH_LABEL, "op": CLEAN_OP, "regionId": on(0, 2), "group": "run-1",
                  "before": side(true), "after": side(true),
                  "parts": [
                      { "regionId": on(0, 2), "before": side(true), "after": side(true) },
                      { "regionId": on(1, 3), "before": side(true), "after": side(true) },
                      { "regionId": on(0, 4), "before": side(true), "after": side(true) },
                  ] },
                { "seq": 3, "label": DETECT_LABEL, "op": DETECTION_OP, "regionId": "", "group": "run-2",
                  "pages": [page_step(1), page_step(0)] },
                { "seq": 4, "label": "e4", "op": REGION_OP, "regionId": on(1, 5), "before": side(true), "after": side(false) },
            ],
        });
        std::fs::create_dir_all(sidecar_dir(job.path())).unwrap();
        std::fs::write(journal_path(job.path()), serde_json::to_vec(&v1).unwrap()).unwrap();

        let mut journal = load_for(job.path(), CHAPTER);
        assert_eq!(journal.version, JOURNAL_VERSION);
        let shape: Vec<_> = journal.entries.iter()
            .map(|entry| (entry.seq, entry.label.as_str(), entry.page, entry.undone))
            .collect();
        assert_eq!(shape, [
            (1, "e1", Some(0), false),
            (2, CLEAN_BATCH_LABEL, Some(0), false),
            (5, CLEAN_BATCH_LABEL, Some(1), false),
            (3, DETECT_LABEL, Some(0), true),
            (6, DETECT_LABEL, Some(1), true),
            (4, "e4", Some(1), true),
        ]);
        assert_eq!(journal.entries[1].parts.iter().map(|part| part.region_id.clone()).collect::<Vec<_>>(), [on(0, 2), on(0, 4)]);
        assert_eq!((journal.entries[2].region_id.clone(), journal.entries[2].parts.len()), (on(1, 3), 0));
        assert_eq!(journal.entries[3].pages[0].source_idx, 1, "source 1 is page 0");
        assert_eq!(journal.entries[4].pages[0].source_idx, 0);
        assert_eq!((journal.cursor, journal.next_seq), (3, 7));

        // Each page steers by its own marks, never by the cursor.
        assert_eq!(journal.undo(0).unwrap().seq, 2);
        assert_eq!(journal.next(1, "redo").unwrap().seq, 6);
        save(job.path(), &journal).unwrap();
        let read = load_for(job.path(), CHAPTER);
        assert_eq!(read.entries, journal.entries, "version 2 reads back as written");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A Detect run over two pages records a step on each, and its later
    /// write of a page joins that page's step.
    #[test]
    fn a_runs_pages_over_two_pages_are_a_step_on_each() {
        use cleaner_core::project::DetectedRegion;
        let (root, mut job) = two_page_job("group-pages");
        let detection = |id: &str, source_idx: usize| DetectedRegion {
            id: id.into(), source_idx, bbox: cleaner_core::mask::Rect::new(4, 6, 12, 12), mask_ref: String::new(),
            inside: true, balloon_color: None, script: None, pick: "fill".into(), detector: "local".into(),
            created: 0, mask_sha256: String::new(), order: 0, review_state: None, fit: None, group: None,
            padding_px: 0, padding_from_seed: false,
        };
        let before = capture_pages(&job, &[0, 1]);
        job.project.detections.push(detection("chapter-p002-d1", 0));
        job.project.detections.push(detection("chapter-p001-d1", 1));
        record_pages(&job, CHAPTER, DETECT_LABEL, Some("run-1"), before);
        let before = capture_pages(&job, &[1]);
        job.project.detections.push(detection("chapter-p001-d2", 1));
        record_pages(&job, CHAPTER, DETECT_LABEL, Some("run-1"), before);

        let mut journal = load_for(job.path(), CHAPTER);
        let shape: Vec<_> = journal.entries.iter()
            .map(|entry| (entry.page, entry.pages.iter().map(|step| step.source_idx).collect::<Vec<_>>()))
            .collect();
        assert_eq!(shape, [(Some(1), vec![0]), (Some(0), vec![1])]);
        assert_eq!(journal.undo(0).unwrap().pages[0].source_idx, 1);
        assert!(journal.undo(0).is_none(), "page 0's part is one step");
        assert!(!journal.entries[0].undone);
        save(job.path(), &journal).unwrap();

        // Without a group, an edit of two pages is a step on each too.
        let before = capture_pages(&job, &[0, 1]);
        job.project.detections.clear();
        record_pages(&job, CHAPTER, DELETE_DETECTION_LABEL, None, before);
        let journal = load_for(job.path(), CHAPTER);
        assert_eq!(journal.entries.iter().filter(|entry| entry.label == DELETE_DETECTION_LABEL).count(), 2);
        assert_eq!(labels(&journal).len(), 3, "the push on page 0 dropped page 0's undone step");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A stored value this build cannot use is the default, not an error and
    /// not a journal of the wrong size.
    #[test]
    fn an_invalid_stored_limit_falls_back_to_the_default() {
        use serde_json::json;
        assert_eq!(limit_from(&json!({})), UNDO_LIMIT_DEFAULT);
        assert_eq!(limit_from(&serde_json::Value::Null), UNDO_LIMIT_DEFAULT);
        for bad in [json!("800"), json!(19), json!(2_001), json!(-5), json!(250.5), json!(null), json!(true), json!([800])] {
            assert_eq!(limit_from(&json!({ "undoLimit": bad })), UNDO_LIMIT_DEFAULT, "{bad}");
        }
        for good in [UNDO_LIMIT_MIN, 800, UNDO_LIMIT_MAX] {
            assert_eq!(limit_from(&json!({ "undoLimit": good })), good);
        }
        assert_eq!(
            undo_limit_of(&json!({ "undoLimit": 800 })),
            UndoLimit { value: 800, min: UNDO_LIMIT_MIN, max: UNDO_LIMIT_MAX, default: UNDO_LIMIT_DEFAULT }
        );
    }

    /// One push as the application does it - read the file, push, write the
    /// file - on a journal filled to each limit with the entries of a real
    /// one. Ignored: it needs a `history.json` from a real chapter, named in
    /// `MANGA_CLEANER_HISTORY_SAMPLE`, and its answer is a time, not a pass.
    #[test]
    #[ignore]
    fn a_push_is_timed_on_a_real_journal() {
        let sample = std::env::var("MANGA_CLEANER_HISTORY_SAMPLE").expect("MANGA_CLEANER_HISTORY_SAMPLE");
        let real: Journal = serde_json::from_slice(&std::fs::read(&sample).unwrap()).unwrap();
        assert!(!real.entries.is_empty(), "the sample journal has no entries");
        let dir = std::env::temp_dir().join(format!("mtclean-history-timing-{}", std::process::id()));
        let job = dir.join("chapter.mtclean");
        for limit in [UNDO_LIMIT_DEFAULT, UNDO_LIMIT_MAX, 5_000] {
            let journal = Journal {
                entries: real.entries.iter().cycle().take(limit).cloned().collect(),
                cursor: limit,
                next_seq: limit as u64 + 1,
                ..Default::default()
            };
            save(&job, &journal).unwrap();
            let bytes = std::fs::metadata(journal_path(&job)).unwrap().len();
            let mut times: Vec<f64> = (0..15)
                .map(|_| {
                    let entry = NewDelta {
                        parts: Vec::new(),
                        label: real.entries[0].label.clone(),
                        op: real.entries[0].op.clone(),
                        region_id: real.entries[0].region_id.clone(),
                        before: real.entries[0].before.clone(),
                        after: real.entries[0].after.clone(), ..Default::default() };
                    let started = std::time::Instant::now();
                    let mut journal = load(&job);
                    journal.push(CHAPTER, entry, limit);
                    save(&job, &journal).unwrap();
                    started.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            times.sort_by(f64::total_cmp);
            println!(
                "{limit} entries, {bytes} bytes: median {:.1} ms, fastest {:.1} ms, slowest {:.1} ms",
                times[times.len() / 2],
                times[0],
                times[times.len() - 1]
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_journal_survives_a_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("mtclean-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let job = dir.join("chapter.mtclean");

        let mut journal = Journal::default();
        push(&mut journal, "masks.command.deleteMask", "r1");
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
