//! How a Nano Banana answer is finished, and where it is kept.
//!
//! A Nano Banana answer is finished by the same choice as every other
//! engine: Settings > Post-processing (`cloudFinishing` and
//! `cloudFinishingGrayscale`, [`from_settings`]). It is always finished, as a
//! FLUX 3 answer is, whatever `finishingEveryModel` says. It had its own three
//! settings before (`nanoBananaFinishing`, `nanoBananaFinishingGrayscale`,
//! `nanoBananaStrays`); a file that still holds them is read without them
//! ([`forget_old_settings`]), and nothing reads them.
//!
//! What a render used is kept in its patch's record
//! (`params_snapshot["postprocess"]`), and a Try again on a Nano Banana layer
//! finishes the new answer the same way ([`recorded`]). A layer's own
//! finishing, stray marks included, is still changed from its previews in the
//! Post-processing window (`inference::refinish`).
//!
//! The answer itself is kept in the chapter's folder, named by its digest
//! ([`keep_answer`]), and the record names it (`nano_banana.answer_sha256`).
//! Changing a Nano Banana layer's finishing afterwards
//! (`inference::refinish`) finishes that answer again: no new request.

use std::path::{Path, PathBuf};

use cleaner_core::engines::postprocess::Choice;
use cleaner_core::ingest::sha256_hex;
use cleaner_core::patch::{Engine, Provenance};
use serde_json::Value;

/// The settings Nano Banana had of its own, before it took the shared one.
const OLD_KEYS: [&str; 3] = ["nanoBananaFinishing", "nanoBananaFinishingGrayscale", "nanoBananaStrays"];

/// The finishing a new Nano Banana answer takes: the shared choice. An
/// unreadable one is the default, as a region edit reads it.
pub fn from_settings(settings: &Value) -> Choice {
    crate::inference::cloud_clean::finishing_from_settings(settings).map(|setting| setting.choice).unwrap_or_default()
}

/// Take the old settings out of a file as it is read, so no reader and no
/// screen sees them, and the next write leaves them out.
pub fn forget_old_settings(settings: &mut Value) {
    if let Some(map) = settings.as_object_mut() {
        for key in OLD_KEYS {
            map.remove(key);
        }
    }
}

/// The finishing a Nano Banana patch was made with, from its record: what a
/// Try again on it finishes the new answer by. `None` for another engine's
/// patch, or one from before the choice was recorded.
pub fn recorded(provenance: &Provenance) -> Option<Choice> {
    (provenance.engine == Engine::NanoBanana)
        .then(|| crate::inference::refinish::recorded_choice(&provenance.params_snapshot))
        .flatten()
}

/// Where a chapter keeps its Nano Banana answers: in its own folder
/// (`Job::sidecar`), one file per answer, named by its digest. Opening the
/// chapter deletes the ones no patch or kept revision names any more
/// (`Job::open`).
const ANSWERS_DIR: &str = cleaner_core::project::NANO_BANANA_ANSWERS_DIR;

fn answer_path(chapter: &Path, sha256: &str) -> PathBuf {
    chapter.join(ANSWERS_DIR).join(sha256)
}

/// Keep `answer` in `chapter`'s folder and return its digest, which the
/// patch's record names. The same answer, shared by regions of one crop, is
/// kept once.
pub fn keep_answer(chapter: &Path, answer: &[u8]) -> Result<String, String> {
    let sha256 = sha256_hex(answer);
    let path = answer_path(chapter, &sha256);
    if path.is_file() {
        return Ok(sha256);
    }
    let dir = chapter.join(ANSWERS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let partial = dir.join(format!("{sha256}.partial"));
    std::fs::write(&partial, answer).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    Ok(sha256)
}

/// The answer a patch's record names, if it is still kept and is still the
/// bytes that digest names.
pub fn kept_answer(chapter: &Path, params: &Value) -> Option<Vec<u8>> {
    let sha256 = params.get("nano_banana")?.get("answer_sha256")?.as_str()?;
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let answer = std::fs::read(answer_path(chapter, sha256)).ok()?;
    (sha256_hex(&answer) == sha256).then_some(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::engines::postprocess::Finishing;
    use serde_json::json;

    /// **Nano Banana takes the shared finishing.** Its old settings do not
    /// reach it, and a file that holds them is read without them.
    #[test]
    fn finishing_is_the_shared_choice_and_the_old_settings_are_gone() {
        assert_eq!(from_settings(&json!({})), Choice::default());
        let mut settings = json!({"cloudFinishing": "poisson", "cloudFinishingGrayscale": true,
            "nanoBananaFinishing": "tone-only", "nanoBananaFinishingGrayscale": false, "nanoBananaStrays": true});
        assert_eq!(from_settings(&settings), Choice::of(Finishing::Poisson, true));
        forget_old_settings(&mut settings);
        assert_eq!(settings, json!({"cloudFinishing": "poisson", "cloudFinishingGrayscale": true}));
        // Unreadable is the default, not a refused edit.
        assert_eq!(from_settings(&json!({"cloudFinishing": "blur"})), Choice::default());
    }

    #[test]
    fn only_a_nano_banana_patch_has_a_recorded_choice() {
        let mut provenance: Provenance = serde_json::from_value(json!({
            "engine": "nanobanana", "engine_version": "x", "model_sha256": null, "execution_provider": "gemini",
            "params_snapshot": {"postprocess": {"finishing": "dots-only", "grayscale": true, "strays": true}},
            "mask_sha256": "", "source_sha256": "", "cloud": null, "created": 0,
        })).unwrap();
        assert_eq!(recorded(&provenance), Some(Choice { finishing: Finishing::DotsOnly, grayscale: true, strays: true }));
        provenance.params_snapshot = json!({});
        assert_eq!(recorded(&provenance), None, "a patch from before the choice was recorded");
        provenance.params_snapshot = json!({"postprocess": {"finishing": "copy"}});
        provenance.engine = Engine::Lama;
        assert_eq!(recorded(&provenance), None, "another engine's");
    }

    #[test]
    fn a_kept_answer_reads_back_by_its_digest_and_only_unchanged() {
        let chapter = std::env::temp_dir().join(format!("mc-nb-answers-{}", std::process::id()));
        let sha256 = keep_answer(&chapter, b"answer bytes").unwrap();
        assert_eq!(keep_answer(&chapter, b"answer bytes").unwrap(), sha256, "kept once");
        let params = json!({"nano_banana": {"answer_sha256": sha256}});
        assert_eq!(kept_answer(&chapter, &params).as_deref(), Some(&b"answer bytes"[..]));
        assert_eq!(kept_answer(&chapter, &json!({"nano_banana": {"answer_sha256": "../x"}})), None);
        assert_eq!(kept_answer(&chapter, &json!({})), None);
        std::fs::write(answer_path(&chapter, &sha256), b"changed").unwrap();
        assert_eq!(kept_answer(&chapter, &params), None, "a changed file is not the answer");
        std::fs::remove_dir_all(&chapter).ok();
    }
}
