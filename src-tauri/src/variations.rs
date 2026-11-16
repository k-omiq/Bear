//! A region's variations: the answers its models have given it, any of which
//! can be made its live result again.
//!
//! **No store of its own.** Every result a region has had is kept already: a
//! commit pushes the row it replaces, and itself, onto the job's revision
//! lists (`Job::complete_region_with_policy`), and nothing prunes them for an
//! ordinary region. A variation is read from those rows:
//!
//! - **Which engines.** FLUX on this computer (any local model), a cloud
//!   render (any profile), Nano Banana, and LaMa. A fill, a screentone fill, a
//!   paint or a clone stroke is arithmetic, so their rows are left out.
//! - **LaMa answers one hole one way**, so its answers are told apart by the
//!   hole: the first clean is one, and each Try again wider, which grows the
//!   hole, is the next. Nothing makes n of them at once.
//! - **One answer, one variation.** Rows are grouped by the answer they show:
//!   Nano Banana's `answer_sha256`, a cloud render's attempt (its request id,
//!   then the revision, when an older row has none), a local FLUX render's
//!   model and seed (the revision, for a row made before seeds were recorded),
//!   a LaMa render's model and hole ([`LAMA_HOLE_KEY`], then the digest of
//!   its mask).
//!   A change of finishing or an erase on an answer is a second row of the
//!   same answer, and the newest row stands for it, so it shows as the user
//!   last left it and does not count twice. The live row stands for its own.
//! - **In the order the answers were first made**, which is the order of the
//!   revision lists.
//!
//! Try again on one of these engines is a new variation: cloud and Nano
//! Banana sample by themselves, and a local FLUX re-run asks at the region's
//! next seed ([`next_flux_seed`]). Picking one ([`crate::library::Library::pick_variation`])
//! renders nothing and keeps everything: one undo step of the region's page.
//!
//! **Making a variation is never an undo step**, and no undo or redo takes
//! one off a region's list: undo puts back which variation is live, and the
//! rows stay where they are kept. A clean run can make more than one
//! generation of each region it cleans ([`GENERATIONS_KEY`]): the first is
//! the region's result as at one generation, and the rest are kept as
//! variations only ([`keep_generation`]), applied by nobody and journaled
//! nowhere.

use cleaner_core::patch::{Engine, Patch};
use cleaner_core::project::{Job, PatchRecord, Project};
use cleaner_core::text_shape::GeometryPolicy;
use serde::Serialize;

/// Where a local FLUX result records the seed it was asked at.
pub(crate) const FLUX_SEED_KEY: &str = "flux_seed";

/// Where a LaMa result whose mask was changed in place (an erase) keeps the
/// digest of the mask it was rendered through, so the erased row still counts
/// as the answer it shows. A row never erased has no such key: its
/// `mask_sha256` is that digest. Not `lama_hole`, which is the hole mode the
/// render ran with (`run::LAMA_HOLE_SNAPSHOT_KEY`).
pub(crate) const LAMA_HOLE_KEY: &str = "lama_rendered_mask_sha256";

/// Call before a row's mask is changed in place. A LaMa row keeps the digest
/// of the mask it was rendered through ([`LAMA_HOLE_KEY`]); the first one
/// recorded stays. Other engines name their answer some other way.
pub(crate) fn pin_lama_hole(provenance: &mut cleaner_core::patch::Provenance) {
    if provenance.engine != Engine::Lama || provenance.mask_sha256.is_empty() {
        return;
    }
    if provenance.params_snapshot.is_null() {
        provenance.params_snapshot = serde_json::json!({});
    }
    if let Some(snapshot) = provenance.params_snapshot.as_object_mut() {
        snapshot.entry(LAMA_HOLE_KEY).or_insert_with(|| serde_json::json!(provenance.mask_sha256));
    }
}

/// Text cleanup's **Generations per region**: how many generations a clean
/// run makes of each region it cleans with an engine that has variations
/// (local FLUX, a Nano Banana batch). A whole number from
/// [`MIN_GENERATIONS`] to [`MAX_GENERATIONS`]; absent or null is
/// [`DEFAULT_GENERATIONS`]. A cloud GPU run binds the count into its plan and
/// shows regions times generations renders before any is sent.
pub const GENERATIONS_KEY: &str = "generationsPerRegion";
pub const MIN_GENERATIONS: u32 = 1;
pub const MAX_GENERATIONS: u32 = 8;
pub const DEFAULT_GENERATIONS: u32 = 1;

/// The setting as `settings` holds it, strictly: a value that is not a whole
/// number in range is refused, not read as the default.
pub(crate) fn generations_from_settings(settings: &serde_json::Value) -> Result<u32, String> {
    match settings.get(GENERATIONS_KEY) {
        None | Some(serde_json::Value::Null) => Ok(DEFAULT_GENERATIONS),
        Some(value) => generations_from_value(value),
    }
}

/// One count of generations, as a command or the settings file carries it.
pub(crate) fn generations_from_value(value: &serde_json::Value) -> Result<u32, String> {
    value.as_u64()
        .and_then(|count| u32::try_from(count).ok())
        .filter(|count| (MIN_GENERATIONS..=MAX_GENERATIONS).contains(count))
        .ok_or_else(|| format!("{GENERATIONS_KEY} must be a whole number from {MIN_GENERATIONS} to {MAX_GENERATIONS}"))
}

/// Keep `extra`, a further generation of the region whose live row is
/// `first`, as a variation only: it is committed, which keeps it on the
/// region's revision list as every commit keeps its row, and `first` is made
/// live again exactly as it was, so the region shows what it showed and its
/// undo steps describe it. Nothing is journaled. The caller holds the job's
/// lock. `false`, with the region as it was, when `first` is not the live row.
pub(crate) fn keep_generation(
    job: &mut Job,
    source_idx: usize,
    first: &PatchRecord,
    extra: &Patch,
) -> Result<bool, String> {
    if job.project.patches.iter().find(|row| row.id == first.id) != Some(first)
        || first.geometry_policy != GeometryPolicy::Legacy {
        return Ok(false);
    }
    let mut extra = extra.clone();
    extra.id = first.id.clone();
    extra.order = first.order;
    // The first generation of a detection was cut to the detection's display
    // set when it was committed (`Job::complete_detection`). A further one is
    // rendered through the same hole and its ring, so it is cut to what the
    // first covers: picking it later must not write where the first did not.
    let shown = job.load_patch(first).map_err(|error| error.to_string())?;
    if let Some(clipped) = cleaner_core::project::clipped_to_display(&extra, &shown.mask, &shown.ink) {
        extra = clipped;
    }
    let committed = job.complete_display_region(source_idx, &extra, first.review_state.clone())
        .map_err(|error| error.to_string());
    // Whatever the commit did, the first is put back: a half-written extra
    // must not be what the region shows.
    let restored = job.restore_legacy_row(first).map_err(|error| error.to_string())?;
    if let Some(live) = job.project.patches.iter_mut().find(|row| row.id == first.id) {
        if *live != *first {
            *live = first.clone();
            job.flush().map_err(|error| error.to_string())?;
        }
    }
    committed?;
    Ok(restored)
}

/// The Undo tooltip of a picked variation.
pub const PICK_LABEL: &str = "masks.command.pickVariation";

/// The notice of a Nano Banana answer the region already had.
pub(crate) const SAME_ANSWER_NOTICE: &str = "notice.variations.sameAnswer";

/// One variation, as the region menu lists it. Its position is its place in
/// the list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Variation {
    /// What `pick_variation` names it by: the row's revision.
    pub revision: String,
    /// `flux`, `cloud`, `nanobanana` or `lama`, as `model/masks.js#maskEngine` says.
    pub engine: &'static str,
    /// The model that answered, where the row says: the local FLUX model, the
    /// cloud's model id, the Nano Banana model.
    pub model: Option<String>,
    /// Seconds since the Unix epoch the answer was made.
    pub created: u64,
    /// Whether the region shows it now.
    pub current: bool,
    #[serde(skip)]
    pub(crate) row: PatchRecord,
}

fn is_cloud(row: &PatchRecord) -> bool {
    row.provenance.cloud.is_some() || row.provenance.engine == Engine::Cloud
}

/// Whether a row's engine has variations: the three that answer differently
/// each time they are asked, and LaMa, which answers differently for a wider
/// hole.
pub(crate) fn variable(row: &PatchRecord) -> bool {
    matches!(row.provenance.engine, Engine::Flux | Engine::Cloud | Engine::NanoBanana | Engine::Lama)
}

fn engine_name(row: &PatchRecord) -> &'static str {
    if is_cloud(row) {
        "cloud"
    } else if row.provenance.engine == Engine::NanoBanana {
        "nanobanana"
    } else if row.provenance.engine == Engine::Lama {
        "lama"
    } else {
        "flux"
    }
}

/// The revision a row is restored by, as an undo snapshot names it.
fn revision_of(row: &PatchRecord) -> Option<String> {
    match row.geometry_policy {
        GeometryPolicy::TextShape => row.text_shape_revision_id().filter(|id| !id.is_empty()),
        _ => row.legacy_revision_id(),
    }
}

/// The seed a local FLUX result was asked at, where it was recorded.
pub(crate) fn recorded_flux_seed(row: &PatchRecord) -> Option<u64> {
    row.provenance.params_snapshot.get(FLUX_SEED_KEY).and_then(serde_json::Value::as_u64)
}

fn flux_model_of(row: &PatchRecord) -> Option<&str> {
    row.provenance.params_snapshot.get("flux_model").and_then(serde_json::Value::as_str)
}

fn answer_sha(row: &PatchRecord) -> Option<&str> {
    row.provenance.params_snapshot.pointer("/nano_banana/answer_sha256")
        .and_then(serde_json::Value::as_str)
        .filter(|sha| !sha.is_empty())
}

/// Which answer a row shows.
fn identity(row: &PatchRecord, revision: &str) -> String {
    if is_cloud(row) {
        let cloud = row.provenance.cloud.as_ref();
        return match cloud.and_then(|cloud| cloud.attempt_id.as_deref()).filter(|id| !id.is_empty())
            .or_else(|| cloud.map(|cloud| cloud.request_id.as_str()).filter(|id| !id.is_empty())) {
            Some(id) => format!("cloud:{id}"),
            None => format!("revision:{revision}"),
        };
    }
    if row.provenance.engine == Engine::NanoBanana {
        return match answer_sha(row) {
            Some(sha) => format!("nanobanana:{sha}"),
            None => format!("revision:{revision}"),
        };
    }
    if row.provenance.engine == Engine::Lama {
        let pinned = row.provenance.params_snapshot.get(LAMA_HOLE_KEY).and_then(serde_json::Value::as_str);
        let model = row.provenance.model_sha256.as_deref().unwrap_or("");
        return match pinned.or(Some(row.provenance.mask_sha256.as_str())).filter(|hole| !hole.is_empty()) {
            Some(hole) => format!("lama:{model}:{hole}"),
            None => format!("revision:{revision}"),
        };
    }
    match (recorded_flux_seed(row), flux_model_of(row)) {
        (Some(seed), Some(model)) => format!("flux:{model}:{seed}"),
        _ => format!("revision:{revision}"),
    }
}

fn model_of(row: &PatchRecord) -> Option<String> {
    if let Some(cloud) = row.provenance.cloud.as_ref() {
        return Some(cloud.model.clone()).filter(|model| !model.is_empty());
    }
    if row.provenance.engine == Engine::NanoBanana {
        return row.provenance.params_snapshot.pointer("/nano_banana/model")
            .and_then(serde_json::Value::as_str).map(str::to_owned);
    }
    flux_model_of(row).map(str::to_owned)
}

/// A region's variations in `project`. Empty for a region with no live
/// result. The live row's answer is `current`, shown as it is live.
pub(crate) fn list(project: &Project, region_id: &str) -> Vec<Variation> {
    list_in(&project.patches, &project.legacy_patch_revisions, &project.text_shape_patch_revisions, region_id)
}

pub(crate) fn list_in(
    patches: &[PatchRecord],
    legacy: &[PatchRecord],
    text_shape: &[PatchRecord],
    region_id: &str,
) -> Vec<Variation> {
    let Some(live) = patches.iter().find(|row| row.id == region_id && row.visible) else {
        return Vec::new();
    };
    let mut groups: Vec<(String, Variation)> = Vec::new();
    for row in legacy.iter().chain(text_shape).chain(std::iter::once(live)) {
        if row.id != region_id || !variable(row) {
            continue;
        }
        let Some(revision) = revision_of(row) else { continue };
        let key = identity(row, &revision);
        let variation = Variation {
            revision,
            engine: engine_name(row),
            model: model_of(row),
            created: row.provenance.created,
            current: false,
            row: row.clone(),
        };
        match groups.iter_mut().find(|(held, _)| *held == key) {
            // The newest row of an answer stands for it, made when it was first made.
            Some((_, held)) => *held = Variation { created: held.created.min(variation.created), ..variation },
            None => groups.push((key, variation)),
        }
    }
    if variable(live) {
        if let Some(revision) = revision_of(live) {
            let key = identity(live, &revision);
            if let Some((_, held)) = groups.iter_mut().find(|(held, _)| *held == key) {
                held.current = true;
                held.revision = revision;
                held.row = live.clone();
            }
        }
    }
    groups.into_iter().map(|(_, variation)| variation).collect()
}

/// The seed a local FLUX render of `region_id` on `model` asks at: the base
/// seed plus the answers the region already has from that model, so a first
/// clean asks at the base seed as it always has and each Try again asks at
/// one the region has not had. A seed already used (an older row's) is
/// skipped.
pub(crate) fn next_flux_seed(project: &Project, region_id: &str, model: &str) -> u64 {
    next_flux_seed_in(project.patches.iter().chain(&project.legacy_patch_revisions)
        .chain(&project.text_shape_patch_revisions), region_id, model)
}

fn next_flux_seed_in<'a>(rows: impl Iterator<Item = &'a PatchRecord>, region_id: &str, model: &str) -> u64 {
    let mut answers: Vec<String> = Vec::new();
    let mut used: Vec<u64> = Vec::new();
    for row in rows {
        if row.id != region_id || row.provenance.engine != Engine::Flux || is_cloud(row)
            || flux_model_of(row) != Some(model) {
            continue;
        }
        let key = identity(row, &revision_of(row).unwrap_or_else(|| row.buffer_ref.clone()));
        if !answers.contains(&key) {
            answers.push(key);
        }
        used.push(recorded_flux_seed(row).unwrap_or(cleaner_core::engines::flux::SEED));
    }
    let mut seed = cleaner_core::engines::flux::SEED + answers.len() as u64;
    while used.contains(&seed) {
        seed += 1;
    }
    seed
}

/// Whether the region's live Nano Banana answer is one an earlier row of it
/// already shows: a Try again that brought no new variation.
pub(crate) fn repeats_an_answer(project: &Project, region_id: &str) -> bool {
    repeats_an_answer_in(&project.patches,
        project.legacy_patch_revisions.iter().chain(&project.text_shape_patch_revisions), region_id)
}

fn repeats_an_answer_in<'a>(patches: &[PatchRecord], kept: impl Iterator<Item = &'a PatchRecord>, region_id: &str)
    -> bool {
    let Some(live) = patches.iter().find(|row| row.id == region_id) else { return false };
    let Some(sha) = answer_sha(live).filter(|_| live.provenance.engine == Engine::NanoBanana) else { return false };
    let revision = revision_of(live);
    kept.into_iter().any(|row| row.id == region_id && revision_of(row) != revision && answer_sha(row) == Some(sha))
}

/// What `pickVariation` answers: the region as it now is, its page's status,
/// and the chapter's history index after the step.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedVariation {
    pub region: crate::library::ApiRegion,
    pub page_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<crate::history::HistoryView>,
}

/// `regionVariations`: a region's variations, in the order they were made.
#[tauri::command]
pub async fn region_variations(app: tauri::AppHandle, region_id: String) -> Result<Vec<Variation>, String> {
    crate::library::blocking(move || Ok(crate::library::Library::for_app(&app)?.region_variations(&region_id)?)).await
}

/// `pickVariation`: make one of a region's variations its live result, one
/// undo step. `null` for a revision that is not one of them.
#[tauri::command]
pub async fn pick_variation(
    app: tauri::AppHandle,
    region_id: String,
    revision: String,
) -> Result<Option<PickedVariation>, String> {
    crate::library::blocking(move || {
        Ok(crate::library::Library::for_app(&app)?
            .pick_variation(&region_id, &revision)?
            .map(|(region, page_status, history)| PickedVariation { region, page_status, history }))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::mask::Rect;
    use cleaner_core::patch::{CloudRecord, Provenance};

    const REGION: &str = "ch1-p001-r1";

    fn row(revision: &str, engine: Engine, snapshot: serde_json::Value, created: u64) -> PatchRecord {
        PatchRecord {
            extra: Default::default(),
            native_parts: Vec::new(),
            native_order: Vec::new(),
            id: REGION.into(),
            source_idx: 0,
            bbox: Rect::new(0, 0, 4, 4),
            mask_ref: format!("patch-revisions/r/{}.mask", hex(revision)),
            buffer_ref: format!("patch-revisions/r/{}.buf", hex(revision)),
            engine,
            order: 0,
            visible: true,
            review_state: None,
            geometry_policy: GeometryPolicy::Legacy,
            text_shape_plan_identity: None,
            provenance: Provenance {
                engine,
                engine_version: "test".into(),
                model_sha256: None,
                execution_provider: "cpu".into(),
                params_snapshot: snapshot,
                mask_sha256: String::new(),
                source_sha256: String::new(),
                cloud: None,
                created,
            },
        }
    }

    fn banana(revision: &str, answer: &str, created: u64) -> PatchRecord {
        row(revision, Engine::NanoBanana,
            serde_json::json!({ "nano_banana": { "answer_sha256": answer, "model": "gemini-x" } }), created)
    }

    fn flux(revision: &str, seed: Option<u64>, created: u64) -> PatchRecord {
        let mut snapshot = serde_json::json!({ "flux_model": "flux2-klein-4b" });
        if let Some(seed) = seed {
            snapshot[FLUX_SEED_KEY] = serde_json::json!(seed);
        }
        row(revision, Engine::Flux, snapshot, created)
    }

    fn cloud(revision: &str, attempt: &str, created: u64) -> PatchRecord {
        let mut cloud = row(revision, Engine::Flux, serde_json::json!({}), created);
        cloud.provenance.cloud = Some(CloudRecord {
            provider: "modal".into(), profile_id: None, job_id: None, request_id: format!("req-{attempt}"),
            attempt_id: Some(attempt.into()), recipe_id: None, model: "klein-9b".into(), model_revision: None,
            tier: None, cost: None, duration_ms: None,
        });
        cloud
    }

    /// A test row's name as the 64 hex digits a revision is.
    fn hex(name: &str) -> String {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(name.as_bytes()))
    }

    fn revisions(list: &[Variation]) -> Vec<(String, bool)> {
        list.iter().map(|variation| (variation.revision.clone(), variation.current)).collect()
    }

    fn want(rows: &[(&str, bool)]) -> Vec<(String, bool)> {
        rows.iter().map(|(name, current)| (hex(name), *current)).collect()
    }

    /// Three Nano Banana answers, the second finished again: three
    /// variations in the order they were made, the second shown as its newest
    /// row, and the live one marked.
    #[test]
    fn rows_are_grouped_by_answer_newest_row_first_made_order() {
        let a = banana("a", "A", 1);
        let b = banana("b", "B", 2);
        let b_finished = banana("b2", "B", 3);
        let c = banana("c", "C", 4);
        let kept = vec![a, b, b_finished, c.clone()];
        let list = list_in(&[c], &kept, &[], REGION);
        assert_eq!(revisions(&list), want(&[("a", false), ("b2", false), ("c", true)]));
        assert_eq!(list[1].created, 2, "an answer is dated when it was first made");
        assert_eq!(list[0].engine, "nanobanana");
        assert_eq!(list[0].model.as_deref(), Some("gemini-x"));
    }

    /// The same row kept twice, and an erase on an answer, are one variation.
    #[test]
    fn duplicate_rows_and_an_erase_collapse_into_their_answer() {
        let a = banana("a", "A", 1);
        let erased = banana("a-erased", "A", 2);
        let b = banana("b", "B", 3);
        let kept = vec![a.clone(), a, b.clone(), erased.clone()];
        let list = list_in(&[erased], &kept, &[], REGION);
        assert_eq!(revisions(&list), want(&[("a-erased", true), ("b", false)]));
    }

    /// The live row stands for its answer even when a newer row of it was
    /// undone away.
    #[test]
    fn the_live_row_stands_for_its_own_answer() {
        let a = banana("a", "A", 1);
        let a_finished = banana("a2", "A", 2);
        let kept = vec![a.clone(), a_finished];
        let list = list_in(&[a], &kept, &[], REGION);
        assert_eq!(revisions(&list), want(&[("a", true)]));
    }

    /// Fill, screentone, paint and clone rows are not variations, and a
    /// region showing one of them has none current.
    #[test]
    fn engines_that_are_arithmetic_are_left_out() {
        let kept = vec![
            row("fill", Engine::Fill, serde_json::json!({}), 2),
            row("tone", Engine::Screentone, serde_json::json!({}), 3),
            row("paint", Engine::Paint, serde_json::json!({}), 4),
            row("clone", Engine::Clone, serde_json::json!({}), 5),
            flux("f1", Some(1), 6),
        ];
        let live = row("fill2", Engine::Fill, serde_json::json!({}), 7);
        let list = list_in(&[live], &kept, &[], REGION);
        assert_eq!(revisions(&list), want(&[("f1", false)]));
    }

    fn lama(revision: &str, hole: &str, created: u64) -> PatchRecord {
        let mut lama = row(revision, Engine::Lama, serde_json::json!({}), created);
        lama.provenance.mask_sha256 = hole.into();
        lama
    }

    /// LaMa answers are told apart by the hole: the first clean, then one per
    /// Try again wider. A plain repeat of a hole is the same answer, and so is
    /// an erase on one, whose row keeps the hole it was rendered through.
    #[test]
    fn lama_answers_are_one_per_hole() {
        let first = lama("first", "hole-1", 1);
        let repeat = lama("repeat", "hole-1", 2);
        let wider = lama("wider", "hole-2", 3);
        let mut erased = lama("wider-erased", "hole-2", 4);
        pin_lama_hole(&mut erased.provenance);
        erased.provenance.mask_sha256 = "hole-2-less-a-stroke".into();
        let mut erased_again = erased.clone();
        erased_again.buffer_ref = format!("patch-revisions/r/{}.buf", hex("wider-erased-2"));
        erased_again.mask_ref = format!("patch-revisions/r/{}.mask", hex("wider-erased-2"));
        pin_lama_hole(&mut erased_again.provenance);
        erased_again.provenance.mask_sha256 = "hole-2-less-two-strokes".into();
        let kept = vec![first, repeat, wider, erased, erased_again.clone()];
        let list = list_in(&[erased_again], &kept, &[], REGION);
        assert_eq!(revisions(&list), want(&[("repeat", false), ("wider-erased-2", true)]));
        assert_eq!(list[0].engine, "lama");
        assert_eq!(list[0].model, None);

        // Another LaMa model on the same hole is another answer.
        let mut other = lama("other-model", "hole-1", 5);
        other.provenance.model_sha256 = Some("big-lama".into());
        let kept = vec![lama("first", "hole-1", 1), other.clone()];
        assert_eq!(revisions(&list_in(&[other], &kept, &[], REGION)), want(&[("first", false), ("other-model", true)]));
    }

    /// A LaMa row with no mask digest (an old row) is its own answer, and
    /// only a LaMa row pins a hole.
    #[test]
    fn a_lama_row_with_no_digest_stands_alone_and_other_engines_pin_nothing() {
        let kept = vec![lama("old-1", "", 1), lama("old-2", "", 2)];
        let list = list_in(&[kept[1].clone()], &kept, &[], REGION);
        assert_eq!(revisions(&list), want(&[("old-1", false), ("old-2", true)]));
        let mut banana = banana("a", "A", 1);
        banana.provenance.mask_sha256 = "m".into();
        pin_lama_hole(&mut banana.provenance);
        assert!(banana.provenance.params_snapshot.get(LAMA_HOLE_KEY).is_none());
    }

    /// Cloud renders group by attempt, local FLUX by model and seed, and a
    /// FLUX row with no seed recorded is its own.
    #[test]
    fn cloud_and_local_flux_identities() {
        let kept = vec![
            flux("old", None, 1),
            flux("s1", Some(1), 2),
            flux("s1-finished", Some(1), 3),
            cloud("c1", "attempt-1", 4),
            cloud("c1-finished", "attempt-1", 5),
            cloud("c2", "attempt-2", 6),
        ];
        let live = kept[5].clone();
        let list = list_in(&[live], &kept, &[], REGION);
        assert_eq!(revisions(&list),
            want(&[("old", false), ("s1-finished", false), ("c1-finished", false), ("c2", true)]));
        assert_eq!(list.iter().map(|variation| variation.engine).collect::<Vec<_>>(),
            vec!["flux", "flux", "cloud", "cloud"]);
        assert_eq!(list[2].model.as_deref(), Some("klein-9b"));
    }

    /// A region with no live result has no variations.
    #[test]
    fn a_deleted_region_lists_nothing() {
        let mut live = banana("a", "A", 1);
        live.visible = false;
        assert!(list_in(&[live.clone()], &[live], &[], REGION).is_empty());
    }

    /// A Nano Banana answer is a repeat when an earlier row of the region
    /// shows it; the live row's own kept copy is not one.
    #[test]
    fn a_nano_banana_answer_the_region_had_is_a_repeat() {
        let a = banana("a", "A", 1);
        let b = banana("b", "B", 2);
        assert!(!repeats_an_answer_in(std::slice::from_ref(&b), [a.clone(), b.clone()].iter(), REGION));
        let a_again = banana("a-again", "A", 3);
        assert!(repeats_an_answer_in(std::slice::from_ref(&a_again), [a, b, a_again.clone()].iter(), REGION));
    }

    /// A first clean asks at the base seed; each answer the region has from
    /// the model moves the next one on; another model's answers and a cloud
    /// render do not count; a seed already used is skipped.
    #[test]
    fn the_next_flux_seed_is_the_base_plus_the_answers_so_far() {
        use cleaner_core::engines::flux::SEED;
        assert_eq!(next_flux_seed_in(std::iter::empty(), REGION, "flux2-klein-4b"), SEED);
        let rows = [flux("s1", Some(SEED), 1), flux("s1-finished", Some(SEED), 2)];
        assert_eq!(next_flux_seed_in(rows.iter(), REGION, "flux2-klein-4b"), SEED + 1,
            "a finishing change is not a second answer");
        let mut other = flux("o", Some(SEED), 3);
        other.provenance.params_snapshot["flux_model"] = serde_json::json!("flux2-klein-9b");
        let rows = [flux("s1", Some(SEED), 1), flux("s2", Some(SEED + 1), 2), other, cloud("c", "a", 4)];
        assert_eq!(next_flux_seed_in(rows.iter(), REGION, "flux2-klein-4b"), SEED + 2);
        assert_eq!(next_flux_seed_in(rows.iter(), REGION, "flux2-klein-9b"), SEED + 1);
        let rows = [flux("old", None, 1), flux("s2", Some(SEED + 1), 2)];
        assert_eq!(next_flux_seed_in(rows.iter(), REGION, "flux2-klein-4b"), SEED + 2,
            "a row from before seeds were recorded was asked at the base seed");
    }
}
