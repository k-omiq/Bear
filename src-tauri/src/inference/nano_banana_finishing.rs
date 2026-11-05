//! How a Nano Banana answer is finished before it is written: its own choice,
//! apart from the one LaMa and local FLUX share (`cloudFinishing` with
//! `finishingEveryModel`).
//!
//! Three settings, each null or absent for its default:
//!
//! - `nanoBananaFinishing`: the version ([`Finishing`], kebab-case). The
//!   default is the default finishing, the one a Nano Banana answer always had
//!   while `finishingEveryModel` was off.
//! - `nanoBananaFinishingGrayscale`: whether a colour answer is made grey
//!   first. A grey page reads the answer grey anyway.
//! - `nanoBananaStrays`: whether small marks the model drew on its own inside
//!   the mask are filled from the tone round them first
//!   ([`cleaner_core::engines::postprocess::strays`]). Off by default: on the
//!   stored real answers it took out the white crescent of one and a small
//!   wave dash of another.
//!
//! What a render used is kept in its patch's record
//! (`params_snapshot["postprocess"]`), and a Try again on a Nano Banana layer
//! finishes the new answer the same way ([`recorded`]).
//!
//! The answer itself is kept in the chapter's folder, named by its digest
//! ([`keep_answer`]), and the record names it (`nano_banana.answer_sha256`).
//! Changing a Nano Banana layer's finishing afterwards
//! (`inference::refinish`) finishes that answer again: no new request.

use std::path::{Path, PathBuf};

use cleaner_core::engines::postprocess::{Choice, Finishing};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::patch::{Engine, Provenance};
use serde_json::Value;

pub const FINISHING_KEY: &str = "nanoBananaFinishing";
pub const GRAYSCALE_KEY: &str = "nanoBananaFinishingGrayscale";
pub const STRAYS_KEY: &str = "nanoBananaStrays";

/// Whether a settings patch names any of the three.
pub fn touches(patch: &Value) -> bool {
    [FINISHING_KEY, GRAYSCALE_KEY, STRAYS_KEY].iter().any(|key| patch.get(key).is_some())
}

/// Strict: each key null, absent, or of its type, the version one this build
/// has. A settings write is refused on an error ([`crate::settings`]); a
/// region edit reads an unreadable one as the default.
pub fn from_settings(settings: &Value) -> Result<Choice, String> {
    let set = |key: &str| settings.get(key).filter(|value| !value.is_null());
    let finishing = match set(FINISHING_KEY) {
        None => Finishing::default(),
        Some(value) => serde_json::from_value(value.clone()).map_err(|_| {
            let names: Vec<String> = Finishing::ALL.iter().map(|f| serde_json::json!(f).as_str().unwrap_or_default().to_owned()).collect();
            format!("{FINISHING_KEY} must be one of {names:?}")
        })?,
    };
    let flag = |key: &str| match set(key) {
        None => Ok(false),
        Some(value) => value.as_bool().ok_or_else(|| format!("{key} must be true or false")),
    };
    Ok(Choice { finishing, grayscale: flag(GRAYSCALE_KEY)?, strays: flag(STRAYS_KEY)? })
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
    use serde_json::json;

    #[test]
    fn absent_or_null_is_the_default_finishing() {
        assert_eq!(from_settings(&json!({})).unwrap(), Choice::default());
        assert_eq!(from_settings(&json!({FINISHING_KEY: null, GRAYSCALE_KEY: null, STRAYS_KEY: null})).unwrap(), Choice::default());
    }

    /// **Nano Banana's choice is its own.** The shared finishing, even with
    /// every model taking it, does not reach it.
    #[test]
    fn the_shared_finishing_does_not_reach_it() {
        let settings = json!({"cloudFinishing": "poisson", "cloudFinishingGrayscale": true, "finishingEveryModel": true,
            FINISHING_KEY: "tone-only", STRAYS_KEY: true});
        assert_eq!(from_settings(&settings).unwrap(), Choice { finishing: Finishing::ToneOnly, grayscale: false, strays: true });
    }

    #[test]
    fn an_unknown_version_or_a_flag_that_is_not_a_boolean_is_refused() {
        assert!(from_settings(&json!({FINISHING_KEY: "blur"})).is_err());
        assert!(from_settings(&json!({GRAYSCALE_KEY: "yes"})).is_err());
        assert!(from_settings(&json!({STRAYS_KEY: 1})).is_err());
        // The old names of the ported versions still read.
        assert_eq!(from_settings(&json!({FINISHING_KEY: "reference-poisson"})).unwrap().finishing, Finishing::Poisson);
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
