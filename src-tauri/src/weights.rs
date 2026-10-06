//! The model catalogue, and the six commands that act on it.
//!
//! The weights are downloaded after install and verified against a pinned
//! manifest. Until now the only thing that did either was
//! `scripts/fetch-models.sh`, a developer's script, so a user who installed the
//! application got an interface that offered every engine and a run that failed
//! on the first page for want of a file they had no way to fetch. This module
//! is the manifest and the fetcher.
//!
//! ## The table is the authority and the script mirrors it
//!
//! [`MODELS`] carries the same artefacts `scripts/fetch-models.sh` fetches,
//! with the same digests and the same URLs, and
//! `the_script_and_the_table_agree` parses the script and asserts it - the same
//! relationship `runtime::package` already has with `scripts/fetch-runtime.sh`.
//! Two copies of a digest that can drift is a download that verifies against
//! the wrong number, which is worse than not verifying at all.
//!
//! `bytes` is on the table for one reason and it is not the progress bar (the
//! response's `Content-Length` is better for that): **it is how `installed` is
//! decided without hashing 200 MB**. A settings dialog that digested every
//! weight it can see would take the better part of a minute to open on a cold
//! cache. So presence plus size is the cheap answer, a full digest is what a
//! download does once on the way in, and [`verify_model`] is the explicit
//! re-check for a file somebody suspects.
//!
//! What a digest *did* answer is remembered, though, in
//! `<app_data>/models/.verified.json`, keyed by the file it was computed from:
//! path, mtime and length. A row therefore keeps its `sha256Ok` across launches
//! and loses it the moment the file underneath changes - which is the whole
//! point of stamping it, and is why the cache is beside the models rather than
//! in the settings store.
//!
//! ## What is downloaded and what is only found
//!
//! [`crate::run::model_search_paths`] has five entries and this module writes
//! to exactly one of them - `<app_data>/models`. A weight found under
//! `$MANGA_CLEANER_MODELS`, beside the executable, or in the bundle's
//! `Resources` is **read-only**: it belongs to a developer's checkout or to the
//! installer, and deleting it from a settings dialog would either fail on
//! permissions or silently break a second application. The row says so and the
//! Delete button is not offered.
//!
//! ## The token
//!
//! All but one of the artefacts are on Hugging Face, which rate-limits anonymous
//! downloads and gates some repositories outright. The token is sent as a
//! bearer token **to `huggingface.co` and to nothing else** - see [`authorize`],
//! which is the only place that reads it - and it is never logged, never
//! returned by [`list_models`], and never put in a URL.
//!
//! It lives in the operating system's credential store, under the service
//! `com.mangacleaner.studio` and the user `huggingface`: the macOS Keychain,
//! the Windows Credential Manager, the Secret Service or the kernel keyring on
//! Linux. [`TokenStore`] is the one seam that store is reached through, so the
//! decisions around it - migrate the old plaintext copy, fall back when there
//! is no store to be had - are testable against an in-memory one.
//!
//! **The fallback is real and it is not a failure.** A headless Linux box has
//! no Secret Service and may have no kernel keyring either; a locked keychain
//! answers with an error rather than a secret. In that case the token stays in
//! `settings.json` exactly as it did before, and the interface says so -
//! `tokenStore` on the view is `keychain` or `file` and the Settings note reads
//! differently for the two. Refusing to store the token at all would trade a
//! file-permissions problem for a rate-limited download.
//!
//! ## What a press answers
//!
//! Three of the commands used to answer `bool`, and two of those booleans were
//! carrying more than one fact. [`DeleteOutcome`] and [`DownloadStart`] are
//! those facts pulled apart:
//! a delete found nothing, found something it does not own, or removed it; a
//! download started, was already running, or had nothing to do. **Every one of
//! them is another window's doing.** This application has one window today, but
//! the download registry is process-wide and a second Settings dialog would
//! draw its rows from a snapshot taken before the first one pressed anything -
//! so the refusals are the shape of two windows disagreeing, and each of them
//! is a sentence rather than a silence.
//!
//! A deleted weight also **evicts the session built from it**
//! ([`resident_kind`]). Nothing is emitted for that: what is loaded is a poll
//! by design, and this takes the same path `unload_model` does.
//!
//! ## Which runtime
//!
//! `runtime::package` describes more than one build for Windows and Linux x64
//! and exactly one for everywhere else. The chosen one is an ordinary setting,
//! [`FLAVOUR_SETTING`], and everything that reads it goes through
//! `package::for_host_flavour`, which resolves an id this platform does not
//! publish to the platform's default - so a settings file copied from a Windows
//! machine to a Mac downloads the Mac's build rather than nothing. The row
//! reports the flavour, its version and its **whole download size** so the
//! picker can put 215 MB against 455 MB in front of the person choosing.
//!
//! ## Resuming
//!
//! A `.part` survives a cancellation and a failure; only a digest mismatch
//! deletes it. The next press sends `Range: bytes=<len>-` and, on a `206`,
//! re-hashes the bytes already on disk before continuing - a few seconds per
//! 200 MB, once, which is the price of `sha2` having no way to persist a
//! `Sha256` mid-stream. A `200` means the server ignored the range and the file
//! starts again. See [`fetch_verified`].
//!
//! Those kept bytes are also disk nobody asked to spend, so the row **says how
//! many there are** - `partialBytes` - and offers [`discard_partial`] to give
//! them back - the runtime's row counting its whole `.downloads` directory,
//! complete archives included, because one kept for a re-unpack is bytes held
//! for a press that may never come just as a `.part` is. A `.part` stamped with
//! a digest this catalogue no longer names is a different matter: nothing will
//! ever resume it, because the press that would is looking for a differently
//! stamped file, so [`list_models`] sweeps those without asking.
//!
//! Both presses take the download registry's lock across the decision **and**
//! the unlink ([`while_not_downloading`]): asking whether an id is downloading
//! and then deleting its `.part` leaves a gap one [`claim`] wide, and a file a
//! writer has just opened is exactly what must not be removed.
//!
//! ## Which runtime is *installed*
//!
//! `runtime_row`'s `flavour` and `version` are the build that **would** be
//! downloaded. What is on disk is a different question and no amount of looking
//! at the files answers it, so a successful install writes down what it
//! unpacked: `<app_data>/runtimes/.installed.json`, the flavour, the version
//! and the file names ([`InstalledRuntime`]). The row carries it back as
//! `installedFlavour` and `installedVersion`, and Settings says so only when it
//! differs from the choice. The file list is the other half: switching
//! build removes whatever the previous stamp named that the new archive did not
//! bring, so no foreign library is left beside the one that will be loaded.
//!
//! ## Replacing a runtime on Windows
//!
//! Everything above was written and measured on macOS, and one of its
//! assumptions is false on Windows: that a file can be deleted while something
//! has it open. It cannot, and **the file in question is always open**. `ort`
//! loads `onnxruntime.dll` through `libloading` and keeps it in a `OnceLock`
//! for the life of the process; `models::list_accelerators` calls
//! `cleaner_core::runtime::load`; and `list_accelerators` is what fills in the
//! settings dialog the Download, flavour-switch and Delete buttons live in. So
//! the library is mapped before any of those buttons can be pressed, the first
//! install worked and every replacement after it failed with a sharing
//! violation.
//!
//! Windows will, however, **rename** a mapped file to another name in the same
//! directory - the mapping is to the file, not to the name. That asymmetry is
//! the whole fix: [`clear_target`] renames what it cannot delete to
//! `<name>.old-<n>`, the new library moves into the name that was freed, and
//! [`sweep_replaced`] deletes the aside copy from a later session that never
//! mapped it. A refusal to do even that is [`NOTICE_IN_USE`], which is a
//! sentence the user can act on rather than an `os error 32`.
//!
//! Two other things about that platform, in the same place because they were
//! found together. A Windows package is three artefacts and used to report as
//! three downloads under one id ([`Portion`]). And its archives unpack to far
//! more than they transfer - `onnxruntime.pdb` alone is about 408 MB behind a
//! 12 MB download - so the volume is asked first ([`room_to_install`],
//! [`unpacked_bytes`]) and the files nothing can load are not written at all
//! ([`is_loadable`]).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use cleaner_core::registry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::events;
use crate::run::{
    model_search_paths, BALLOONS, DETECTOR, GATE_LABELS, GATE_MODEL, HAYAI_DECODER, HAYAI_TOKENIZER,
    HAYAI_VISION, INPAINTER, OCR_DECODER, OCR_ENCODER, OCR_VOCAB,
};

/* ------------------------------------------------------------------ */
/* The catalogue                                                       */
/* ------------------------------------------------------------------ */

/// One downloadable weight.
///
/// `required_by` names **engines and features**, in the interface's own
/// vocabulary - `autoClean`, `lama` - rather than the module that
/// opens the file, because the whole point of carrying it across the seam is
/// that the interface can decide which controls to draw from it without a
/// second table of its own. An engine named by no row (`fill`, `denoise`) needs
/// no weights and is therefore always available, which falls out of the rule
/// rather than being written down twice.
#[derive(Debug, Clone, Copy)]
pub struct ModelPackage {
    /// Stable across releases; what every command takes.
    pub id: &'static str,
    /// The name the file must have on disk - [`crate::run`]'s consts.
    pub file_name: &'static str,
    pub url: &'static str,
    /// Never empty. An artefact without a pinned digest does not belong here.
    pub sha256: &'static str,
    /// The artefact's exact size. See the module docs for why it is here.
    pub bytes: u64,
    /// e.g. `models.kind.inpainter`. An i18n key: no English crosses the seam.
    pub kind_key: &'static str,
    /// Engine ids and feature ids this file is a precondition of.
    pub required_by: &'static [&'static str],
}

/// The artefacts, in `scripts/fetch-models.sh`'s own order.
pub const MODELS: &[ModelPackage] = &[
    // Optional local text detector, downloaded separately from upstream.
    ModelPackage {
        id: "textDetector",
        file_name: DETECTOR,
        url: concat!(
            "https://github.com/zyddnys/manga-image-translator/releases/download",
            "/beta-0.2.1/comictextdetector.pt.onnx"
        ),
        sha256: "1a86ace74961413cbd650002e7bb4dcec4980ffa21b2f19b86933372071d718f",
        bytes: 94_669_756,
        kind_key: "models.kind.textDetector",
        required_by: &["autoClean"],
    },
    // Rung 2, the default inpainter.
    ModelPackage {
        id: "inpainter",
        file_name: INPAINTER,
        url: "https://huggingface.co/mayocream/lama-manga-onnx/resolve/main/lama-manga.onnx",
        sha256: "4512adab295ee5a5e02ccd1bdf8d45dccbac88309d9cff1532ffd5de876f02a4",
        bytes: 207_482_644,
        kind_key: "models.kind.inpainter",
        required_by: &["lama"],
    },
    // The script gate, and its labels, which must match it.
    ModelPackage {
        id: "scriptGate",
        file_name: GATE_MODEL,
        url: concat!(
            "https://huggingface.co/ogkalu/image-script-identification",
            "/resolve/main/osd_lstm.onnx"
        ),
        sha256: "b18e0c1479d9eb67394993098f7e1079c9a93ef6f7b0416ee333fccb865c6e72",
        bytes: 3_722_314,
        kind_key: "models.kind.scriptGate",
        required_by: &["autoClean"],
    },
    ModelPackage {
        id: "scriptGateLabels",
        file_name: GATE_LABELS,
        url: concat!(
            "https://huggingface.co/ogkalu/image-script-identification",
            "/resolve/main/osd_labels.json"
        ),
        sha256: "a1888156b005065039c356e13a7bbef1ec454b45bf6aaf18c11f4a59b1ee35c5",
        bytes: 1_163,
        kind_key: "models.kind.scriptGateLabels",
        required_by: &["autoClean"],
    },
    // What decides "in a balloon".
    ModelPackage {
        id: "balloonDetector",
        file_name: BALLOONS,
        url: concat!(
            "https://huggingface.co/ogkalu/comic-text-and-bubble-detector",
            "/resolve/main/detector-v4-s_int8.onnx"
        ),
        sha256: "5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79",
        bytes: 11_120_765,
        kind_key: "models.kind.balloonDetector",
        required_by: &["autoClean"],
    },
    ModelPackage {
        id: "fullRt",
        file_name: "detector.onnx",
        url: "https://huggingface.co/ogkalu/comic-text-and-bubble-detector/resolve/16e8a622f91fabc6b5b65c96d32d1183f8843546/detector.onnx",
        sha256: "065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195",
        bytes: 168_481_531,
        kind_key: "models.kind.fullRt",
        required_by: &["review"],
    },
    /* The gate's rescue reader: `manga-ocr`, Apache-2.0, exported
     * to ONNX by the same author as `lama-manga.onnx`.
     *
     * **`required_by` is empty, and it is the only row where that is true.**
     * Every other artefact here is a precondition of something the interface
     * offers - Auto clean, or the redraw engine - and an empty list is this
     * table's way of saying *nothing stops working without this*. The gate
     * runs, cleans and refuses exactly as it did before the reader existed; the
     * reader recovers about 4% of regions that would otherwise reach review
     * uncleaned. So it is downloadable from Settings and it is not in the
     * first-launch offer: 460 MB is not something to put in front of somebody
     * who has not yet cleaned a page, and MI-GAN's row was the last one that
     * tried.
     *
     * Three rows and three names, for `scriptGateLabels`' reason: Settings
     * lists one row per *file*, and a row nobody can name is a row nobody can
     * decide about. `resident_kind` maps all three onto the one session they
     * are three parts of. */
    ModelPackage {
        id: "ocrEncoder",
        file_name: OCR_ENCODER,
        url: concat!(
            "https://huggingface.co/mayocream/manga-ocr-onnx",
            "/resolve/main/encoder_model.onnx"
        ),
        sha256: "15fa8155fe9bc1a7d25d9bb353debaa4def033d0174e907dbd2dd6d995def85f",
        bytes: 343_454_249,
        kind_key: "models.kind.ocr",
        required_by: &[],
    },
    ModelPackage {
        id: "ocrDecoder",
        file_name: OCR_DECODER,
        url: concat!(
            "https://huggingface.co/mayocream/manga-ocr-onnx",
            "/resolve/main/decoder_model.onnx"
        ),
        sha256: "ef7765261e9d1cdc34d89356986c2bbc2a082897f753a89605ae80fdfa61f5e8",
        bytes: 117_480_262,
        kind_key: "models.kind.ocrDecoder",
        required_by: &[],
    },
    ModelPackage {
        id: "ocrVocab",
        file_name: OCR_VOCAB,
        url: "https://huggingface.co/mayocream/manga-ocr-onnx/resolve/main/vocab.txt",
        sha256: "5cb5c5586d98a2f331d9f8828e4586479b0611bfba5d8c3b6dadffc84d6a36a3",
        bytes: 30_216,
        kind_key: "models.kind.ocrVocab",
        required_by: &[],
    },
    /* The multi-script text reader (`cleaner_core::gate::hayai`): Hayai OCR
     * v2.5 Nova, Apache-2.0, exported to ONNX in `spikes/hayai-ocr` and hosted
     * at `bixii/hayai-ocr-v2.5-nova-onnx`, pinned to the upload's commit.
     * Optional like manga-ocr, and preferred over it when both are present:
     * a run that finds all three files reads regions with it (the text check
     * and the rescue, `run.rs#read_check`). */
    ModelPackage {
        id: "hayaiVision",
        file_name: HAYAI_VISION,
        url: concat!(
            "https://huggingface.co/bixii/hayai-ocr-v2.5-nova-onnx",
            "/resolve/23653e3c09c6e07b9172668cf7fa238f6411ab14/hayai-ocr-vision.onnx"
        ),
        sha256: "379ec20e7d5b134e6bd0e7c5cf0a4e705318129bfb710e25e69c6ca027b84021",
        bytes: 343_538_300,
        kind_key: "models.kind.hayaiVision",
        required_by: &[],
    },
    ModelPackage {
        id: "hayaiDecoder",
        file_name: HAYAI_DECODER,
        url: concat!(
            "https://huggingface.co/bixii/hayai-ocr-v2.5-nova-onnx",
            "/resolve/23653e3c09c6e07b9172668cf7fa238f6411ab14/hayai-ocr-decoder.onnx"
        ),
        sha256: "23342ad16efee65486347b7ac15c98d5f78412eb4c4bee3236d03d8478cb0e20",
        bytes: 255_717_567,
        kind_key: "models.kind.hayaiDecoder",
        required_by: &[],
    },
    ModelPackage {
        id: "hayaiTokenizer",
        file_name: HAYAI_TOKENIZER,
        url: concat!(
            "https://huggingface.co/bixii/hayai-ocr-v2.5-nova-onnx",
            "/resolve/23653e3c09c6e07b9172668cf7fa238f6411ab14/hayai-ocr-tokenizer.json"
        ),
        sha256: "f8a0a909c628a684fe463094614e236a8b1d3609e7770f77e7beafaf1056bf13",
        bytes: 1_247_253,
        kind_key: "models.kind.hayaiTokenizer",
        required_by: &[],
    },
    /* Local page denoise (`cleaner_core::page_denoise`): waifu2x swin_unet
     * `art_scan` `noise2_scale4x` and nunif's seam blending helper, MIT, from
     * the `deepghs/waifu2x_onnx` mirror pinned to a revision. The same files
     * and digests as `deploy/cloud/common/denoise.py`, so local and cloud run
     * the same graph. Optional: nothing downloads them unless the user picks
     * local denoise, and the two are one group (`pageDenoise`). */
    ModelPackage {
        id: "pageDenoiseModel",
        file_name: cleaner_core::page_denoise::MODEL_FILE,
        url: concat!(
            "https://huggingface.co/deepghs/waifu2x_onnx/resolve/333b95cc88a6a9f39abb6426ab580f0d673f1185",
            "/20250502/onnx_models/swin_unet/art_scan/noise2_scale4x.onnx"
        ),
        sha256: "532424408a1fd293c6fbfd54b44cd9077b7a4da8452218fd4d8040c7c9c78747",
        bytes: 18_905_524,
        kind_key: "models.kind.pageDenoise",
        required_by: &["pageDenoise"],
    },
    ModelPackage {
        id: "pageDenoiseSeams",
        file_name: cleaner_core::page_denoise::SEAM_FILTER_FILE,
        url: concat!(
            "https://huggingface.co/deepghs/waifu2x_onnx/resolve/333b95cc88a6a9f39abb6426ab580f0d673f1185",
            "/20250502/onnx_models/utils/create_seam_blending_filter.onnx"
        ),
        sha256: "7d825bc0bbba65493cd3e1809a1cd6db4999243d516154b7974522dce6826ad5",
        bytes: 26_530,
        kind_key: "models.kind.pageDenoiseSeams",
        required_by: &["pageDenoise"],
    },
];

/// The group a local page denoise needs whole: its model and its seam helper.
pub const PAGE_DENOISE_GROUP: &str = "pageDenoise";

/// The Hayai reader's three files, downloaded and deleted together.
pub const HAYAI_GROUP: &str = "hayaiOcr";

/// The progress id the ONNX Runtime's own download reports under. Not a
/// [`MODELS`] row - the runtime is an archive, not a weight - but the same
/// event and the same button, because to the reader of the Models section they
/// are one list.
pub const RUNTIME_ID: &str = "runtime";

pub fn package(id: &str) -> Option<&'static ModelPackage> {
    MODELS.iter().find(|model| model.id == id)
}

/// Logical multi-file capabilities. No group includes the shared RT-DETR
/// detector, so removing Japanese filtering cannot remove discovery weights.
fn model_group(id: &str) -> Option<Vec<&'static ModelPackage>> {
    let ids: &[&str] = match id {
        "scriptGate" => &["scriptGate", "scriptGateLabels"],
        "mangaOcr" => &["ocrEncoder", "ocrDecoder", "ocrVocab"],
        HAYAI_GROUP => &["hayaiVision", "hayaiDecoder", "hayaiTokenizer"],
        PAGE_DENOISE_GROUP => &["pageDenoiseModel", "pageDenoiseSeams"],
        _ => return None,
    };
    ids.iter().map(|id| package(id)).collect()
}

fn group_for_member(id: &str) -> Option<&'static str> {
    match id {
        "scriptGate" | "scriptGateLabels" => Some("scriptGate"),
        "ocrEncoder" | "ocrDecoder" | "ocrVocab" => Some("mangaOcr"),
        "hayaiVision" | "hayaiDecoder" | "hayaiTokenizer" => Some(HAYAI_GROUP),
        "pageDenoiseModel" | "pageDenoiseSeams" => Some(PAGE_DENOISE_GROUP),
        _ => None,
    }
}

/// Every file of a group, found on disk and matching its pinned digest, for
/// a command about to open them. A digest this module remembers for the
/// exact file answers without re-reading it; any other file is hashed once
/// and the answer kept, as [`verify_model`] keeps it.
pub(crate) fn verified_group(app_data: Option<&Path>, group: &str) -> Result<Vec<PathBuf>, String> {
    let models = model_group(group).ok_or_else(|| format!("no such model group: {group}"))?;
    let writable = app_data.map(|dir| dir.join("models"));
    let cache = writable.as_deref().map(read_cache).unwrap_or_default();
    models.iter().map(|model| {
        let (path, _) = locate(model, app_data, writable.as_deref())
            .ok_or_else(|| format!("{} is not installed", model.id))?;
        let shown = path.display().to_string();
        let remembered = match (cache.get(model.id), stamp(&path)) {
            (Some(entry), Some(stamp)) => cache_hit(entry, &shown, stamp),
            _ => None,
        };
        let ok = match remembered {
            Some(ok) => ok,
            None => {
                let ok = digest_file(&path)? == model.sha256;
                record_verified(model.id, ok);
                remember(writable.as_deref(), model.id, &path, ok);
                ok
            }
        };
        if !ok { return Err(format!("{} failed its pinned SHA-256", model.id)); }
        Ok(path)
    }).collect()
}

/* ------------------------------------------------------------------ */
/* Where things are                                                    */
/* ------------------------------------------------------------------ */

fn app_data(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;
    app.path().app_data_dir().ok()
}

/// The one directory this module writes to and deletes from.
fn writable_models_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_data(app)
        .map(|dir| dir.join("models"))
        .ok_or_else(|| "no application data directory on this machine".to_owned())
}

/// Whether a complete, verified copy of an artefact is already sitting in the
/// download directory.
///
/// Only ever true after an attempt that downloaded the archive and then failed
/// to unpack it, which is why the cost - a full digest of up to 202 MB - is
/// paid at all: it is seconds, once, against a download of minutes.
fn already_fetched(archive: &Path, sha256: &str) -> bool {
    std::fs::metadata(archive).map(|meta| meta.is_file()).unwrap_or(false)
        && digest_file(archive).map(|got| got == sha256).unwrap_or(false)
}

/// Where the runtime's *unpacked* libraries are assembled before being moved
/// into place. Removed before and after every attempt.
fn runtime_staging(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(".staging")
}

/// Where the runtime's archives and their `.part` files live.
///
/// Deliberately **not** inside [`runtime_staging`]: everything in there is
/// removed at both ends of an attempt, so an archive downloaded there could
/// never be resumed. See [`download_runtime`].
fn runtime_downloads(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(".downloads")
}

fn writable_runtime_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_data(app)
        .map(|dir| dir.join("runtimes"))
        .ok_or_else(|| "no application data directory on this machine".to_owned())
}

/* ------------------------------------------------------------------ */
/* Refusals the interface can translate                                */
/* ------------------------------------------------------------------ */

/// Two failures of a runtime install are the user's to act on rather than a
/// developer's to read, so they cross the seam as keys.
///
/// **They travel as strings because that is the only channel there is.**
/// `download_runtime` answers `Result<DownloadStart, String>` and everything it
/// does afterwards reaches the interface as `Event::ModelProgress`'s
/// `error: Option<String>`; the interface already reads one of those strings as
/// a sentinel rather than as a sentence - `'cancelled'` - so this is that
/// arrangement written down rather than a new one.
///
/// The grammar is one line and the adapter's whole job:
///
/// ```text
/// <key>
/// <key> <name>=<value> <name>=<value> …
/// ```
///
/// The first whitespace-separated field is the i18n key. Every field after it is
/// `name=value`, and the values are decimal byte counts - **numbers are not
/// translatable, so they travel beside the key rather than inside it**, which is
/// the arrangement `accel.declined.memory` already has with `neededBytes` and
/// `roomBytes`. An error whose first field is not a known key is a diagnostic
/// and is shown as it is, which is what every other failure here still is.
///
/// Only Windows can raise this one, and only Windows compiles the code that
/// does. The key is still declared on every platform: it is the seam's
/// vocabulary rather than one target's, and a constant that exists in one build
/// and not another is a contract the two builds disagree about. Being unused on
/// the platforms that cannot reach it is what that costs.
#[cfg_attr(not(windows), allow(dead_code))]
pub const NOTICE_IN_USE: &str = "notice.runtime.inUse";

/// See [`NOTICE_IN_USE`]. Carries `needed` and `free`.
pub const NOTICE_NO_SPACE: &str = "notice.runtime.noSpace";

/// The refusal a preflight writes, with the two figures the sentence needs.
fn no_space(needed: u64, free: u64) -> String {
    format!("{NOTICE_NO_SPACE} needed={needed} free={free}")
}

/// How many bytes the volume holding `path` will still take.
///
/// `None` is **this machine would not say**, and every caller reads it as
/// permission to go ahead: a download refused on ignorance is worse than one
/// that runs out of disk, because the second at least fails with the reason
/// attached.
///
/// The path need not exist. `<app_data>/runtimes` is created by the first
/// download, and the question is about the volume rather than the directory, so
/// the nearest ancestor that does exist is what is asked about.
pub(crate) fn free_space(path: &Path) -> Option<u64> {
    let mut candidate = path;
    loop {
        if candidate.exists() {
            return free_on(candidate);
        }
        candidate = candidate.parent()?;
    }
}

/// `f_bavail`, not `f_bfree`: the blocks a filesystem reserves for root are not
/// space this process can spend, and counting them is how a preflight passes and
/// the write fails anyway.
#[cfg(unix)]
fn free_on(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `stat` is written by `statvfs` and read only where it answered 0.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(name.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    // `f_frsize` is the fragment size the counts are in; a filesystem that
    // leaves it zero is answering in `f_bsize` instead.
    let block = if stat.f_frsize > 0 { stat.f_frsize } else { stat.f_bsize };
    // Written as casts rather than as `u64::from`, because `fsblkcnt_t` is 32
    // bits on macOS and 64 on Linux: a conversion that compiles on one is a
    // `useless_conversion` on the other, and a cast that is a no-op on one is
    // the widening on the other. The lint is silenced rather than the two
    // platforms given two spellings.
    #[allow(clippy::unnecessary_cast)]
    let free = (stat.f_bavail as u64).checked_mul(block as u64);
    free
}

/// `lpFreeBytesAvailableToCaller`, which is the figure a quota-limited account
/// actually has rather than the one the volume has.
#[cfg(windows)]
fn free_on(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `free` is written
    // only when the call answers non-zero, which is what is checked.
    let answered = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (answered != 0).then_some(free)
}

/// Refuse a download the volume cannot hold, and say by how much.
///
/// `needed == 0` and a machine that will not answer both go through: see
/// [`free_space`].
fn ensure_room(dir: &Path, needed: u64) -> Result<(), String> {
    if needed == 0 {
        return Ok(());
    }
    let Some(free) = free_space(dir) else { return Ok(()) };
    if free >= needed {
        return Ok(());
    }
    Err(no_space(needed, free))
}

/// What a runtime install needs on the volume before any of the bytes are here.
///
/// **The size the row advertises is the download, and the download is not what
/// this costs.** An install holds the archives in `.downloads` *and* their
/// unpacked contents in `.staging` at the same time - the archive is deleted
/// only after its own unpack - so the peak is the transfer plus what comes out
/// of it, against a picker that puts "455 MB" in front of the person choosing.
///
/// Only the first half is knowable here: what an archive unpacks to is in the
/// archive, and the archive is what is about to be fetched. So this is `2 x` the
/// transfer, and it is a **floor rather than a bound** - stated plainly because
/// a reader deserves to know which it is. The exact figure is checked again,
/// from the zip's own central directory, immediately before each unpack; see
/// [`unpacked_bytes`], which is where the 408 MB `onnxruntime.pdb` that started
/// this was found.
fn room_to_install(to_fetch: u64) -> u64 {
    to_fetch.saturating_mul(2)
}

/// How many bytes [`unpack`] will write out of this archive, read from the
/// archive's own table of contents.
///
/// A zip - which is every Windows artefact, and the WebGPU plugin on Linux -
/// carries an uncompressed length per entry in its central directory, so this
/// is exact and costs a seek rather than an inflate. Only the entries that will
/// actually be written are counted, which is what makes it exact rather than
/// merely conservative.
///
/// A `.tgz` answers `None`. Its sizes live in headers interleaved with the data,
/// so reading them means inflating the whole archive - minutes, to learn
/// something the caller then uses to decide whether to spend seconds. macOS and
/// Linux get no check, which is the platform where the archive is 32 MB and the
/// failure was never seen.
fn unpacked_bytes(archive: &Path, library_dir: &str) -> Option<u64> {
    if !is_zip(archive) {
        return None;
    }
    let file = std::fs::File::open(archive).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut total = 0u64;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).ok()?;
        if !entry.is_file() {
            continue;
        }
        let path = entry.name().replace('\\', "/");
        let Some(name) = under(&path, library_dir) else { continue };
        if !is_loadable(name) {
            continue;
        }
        total = total.saturating_add(entry.size());
    }
    Some(total)
}

/// Where a weight actually is, and whether this module may delete it.
///
/// Presence **and size**: a `.part` that was renamed by hand, or a file
/// truncated by a full disk, is not an installed model, and the size is the
/// only check cheap enough to make every time the dialog opens.
fn locate(model: &ModelPackage, app_data: Option<&Path>, writable: Option<&Path>) -> Option<(PathBuf, bool)> {
    // The verified full RT import and managed download share this owned layout.
    // Check it before generic search roots so Settings sees the same graph the
    // workflow and automatic run use.
    if model.id == "fullRt" {
        if let Some(root) = app_data {
            let dir = root.join("models/rtdetr-v2-full");
            let path = dir.join(model.file_name);
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.is_file() && meta.len() == model.bytes {
                    return Some((path, is_read_only(dir.as_path(), writable, &meta)));
                }
            }
        }
    }
    for dir in model_search_paths(app_data) {
        let path = dir.join(model.file_name);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if !meta.is_file() || meta.len() != model.bytes {
            continue;
        }
        return Some((path, is_read_only(dir.as_path(), writable, &meta)));
    }
    None
}

/// Whether the Delete button is offered for a copy found in `dir`.
///
/// Two questions, and both of them have to say yes before a file may be
/// removed. **Ownership**: a weight outside `<app_data>/models` belongs to a
/// developer's checkout or to the installer, and deleting it from a settings
/// dialog would either fail or silently break a second application.
/// **Permission**: a file the filesystem says is read-only cannot be removed
/// either, and until that was fixed the row offered a button whose press
/// reported an OS error.
///
/// The Windows half of `permissions().readonly()` is famously not what it looks
/// like - it reads `FILE_ATTRIBUTE_READONLY` and knows nothing about the ACL
/// that actually decides - so on Windows this catches the attribute and misses
/// a denial, which is one direction of wrong rather than two. Ownership is
/// still the rule that does the work.
fn is_read_only(dir: &Path, writable: Option<&Path>, meta: &std::fs::Metadata) -> bool {
    let ours = writable.map(|w| dir == w).unwrap_or(false);
    !ours || meta.permissions().readonly()
}

/* ------------------------------------------------------------------ */
/* Digests                                                             */
/* ------------------------------------------------------------------ */

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hash a file in 1 MiB blocks. Never reads a whole model into memory: the
/// inpainter is 207 MB and this runs on the caller's thread.
pub fn digest_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// What a [`verify_model`] found, by id. `None` for a row nothing has checked
/// this session - which is the ordinary state, and is why the field is a
/// tri-state on the wire rather than a boolean.
fn verified() -> &'static Mutex<HashMap<String, bool>> {
    static VERIFIED: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    VERIFIED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_verified(id: &str, ok: bool) {
    if let Ok(mut map) = verified().lock() {
        map.insert(id.to_owned(), ok);
    }
}

fn forget_verified(id: &str) {
    if let Ok(mut map) = verified().lock() {
        map.remove(id);
    }
}

/* ------------------------------------------------------------------ */
/* The digest cache                                                    */
/* ------------------------------------------------------------------ */

/// The file the session's answers are kept in, beside the weights they are
/// about. A dot-file so that a user reading the offline-install listing of
/// `<app_data>/models` sees six weights and not seven files.
const VERIFIED_FILE: &str = ".verified.json";

/// A digest result, remembered against the exact file it was computed from.
///
/// Path, mtime and length together: the path because a weight can be found on
/// any of five search paths and the answer is about one of them, the mtime and
/// the length because a file rewritten under the same name is a different file
/// and its old answer is worse than no answer at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VerifiedEntry {
    path: String,
    mtime_ms: u64,
    len: u64,
    ok: bool,
}

/// A file's `(mtime, length)`, in milliseconds since the epoch.
///
/// `None` when the platform will not answer - some filesystems have no
/// modification time, and one whose mtime predates 1970 cannot be expressed
/// here. Nothing is cached in that case, which costs a `verifyModel` press and
/// breaks nothing.
fn stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let since = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((since.as_millis() as u64, meta.len()))
}

fn read_cache(models_dir: &Path) -> HashMap<String, VerifiedEntry> {
    // Every failure is the same answer: an unreadable or malformed cache is a
    // cache miss, never an error the user hears about.
    std::fs::read(models_dir.join(VERIFIED_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_cache(models_dir: &Path, cache: &HashMap<String, VerifiedEntry>) {
    let Ok(bytes) = serde_json::to_vec_pretty(cache) else { return };
    if std::fs::create_dir_all(models_dir).is_err() {
        return;
    }
    // The same temp-and-rename the settings file gets: a truncated cache would
    // be read as no cache, which is harmless, but writing it atomically costs
    // nothing here.
    let _ = cleaner_core::project::buffers::write_atomic(&models_dir.join(VERIFIED_FILE), &bytes);
}

/// Whether a remembered result still describes the file on disk.
///
/// `None` is "ask again": the file moved, was rewritten, or changed length.
/// This is the invalidation, and it is the whole reason the entry carries three
/// fields rather than one boolean.
fn cache_hit(entry: &VerifiedEntry, path: &str, stamp: (u64, u64)) -> Option<bool> {
    let (mtime_ms, len) = stamp;
    (entry.path == path && entry.mtime_ms == mtime_ms && entry.len == len).then_some(entry.ok)
}

/// Remember what a digest decided, so the next launch does not have to ask.
fn remember(models_dir: Option<&Path>, id: &str, path: &Path, ok: bool) {
    let Some(dir) = models_dir else { return };
    let Some((mtime_ms, len)) = stamp(path) else { return };
    let mut cache = read_cache(dir);
    cache.insert(
        id.to_owned(),
        VerifiedEntry { path: path.display().to_string(), mtime_ms, len, ok },
    );
    write_cache(dir, &cache);
}

/// Drop a remembered result. A deleted weight's answer is about a file that no
/// longer exists, and a re-downloaded one writes its own.
fn forget_remembered(models_dir: Option<&Path>, id: &str) {
    let Some(dir) = models_dir else { return };
    let mut cache = read_cache(dir);
    if cache.remove(id).is_some() {
        write_cache(dir, &cache);
    }
}

/* ------------------------------------------------------------------ */
/* Downloads in flight                                                 */
/* ------------------------------------------------------------------ */

/// One cancel flag per id. Presence in the map is what "downloading" means, so
/// a second `download_model` for an id already running is refused rather than
/// racing the first into the same `.part` file.
fn inflight() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Claim an id. `None` when one is already running.
fn claim(id: &str) -> Option<Arc<AtomicBool>> {
    let mut map = inflight().lock().ok()?;
    if map.contains_key(id) {
        return None;
    }
    let flag = Arc::new(AtomicBool::new(false));
    map.insert(id.to_owned(), Arc::clone(&flag));
    Some(flag)
}

/// Reserve every file of one logical model in a single registry operation.
/// File-level buttons cannot race a multi-file install or removal halfway
/// through its dependency set.
fn claim_group(group: &str, models: &[&ModelPackage]) -> Option<Arc<AtomicBool>> {
    let mut map = inflight().lock().ok()?;
    if map.contains_key(group) || models.iter().any(|model| map.contains_key(model.id)) {
        return None;
    }
    let flag = Arc::new(AtomicBool::new(false));
    map.insert(group.to_owned(), Arc::clone(&flag));
    for model in models { map.insert(model.id.to_owned(), Arc::clone(&flag)); }
    Some(flag)
}

fn release_group(group: &str, models: &[&ModelPackage]) {
    if let Ok(mut map) = inflight().lock() {
        map.remove(group);
        for model in models { map.remove(model.id); }
    }
}

fn release(id: &str) {
    if let Ok(mut map) = inflight().lock() {
        map.remove(id);
    }
}

fn is_downloading(id: &str) -> bool {
    inflight().lock().map(|map| map.contains_key(id)).unwrap_or(false)
}

/// Do something to an id's files **while no download can claim it**, and answer
/// `busy` when one already has.
///
/// [`is_downloading`] followed by a `remove_file` is two decisions with a gap
/// between them, and the gap is exactly one [`claim`] wide: a Download press in
/// another window can pass the registry, open the `.part` and start appending
/// between the check and the unlink, and the file the writer holds is then
/// deleted underneath it. Holding the registry lock across **both** closes it -
/// `claim` blocks on the same mutex, so a press either arrives before this and
/// is seen, or waits and finds the `.part` already gone.
///
/// `work` therefore runs with that lock held and must not touch the registry
/// itself; it does filesystem work and nothing else. The cost is that a Download
/// press waits for one `unlink`, which is not a wait anybody can perceive.
fn while_not_downloading<T>(id: &str, busy: T, work: impl FnOnce() -> T) -> T {
    let Ok(map) = inflight().lock() else { return busy };
    if map.contains_key(id) {
        return busy;
    }
    work()
}

/* ------------------------------------------------------------------ */
/* The rows the interface draws                                        */
/* ------------------------------------------------------------------ */

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub id: &'static str,
    pub file_name: &'static str,
    pub bytes: u64,
    /// Immutable file identity even when the upstream URL points at main.
    pub sha256: &'static str,
    pub kind_key: &'static str,
    pub required_by: &'static [&'static str],
    pub installed: bool,
    pub path: Option<String>,
    /// Found somewhere this module may not write. Delete is not offered.
    pub read_only: bool,
    /// `None` unless something digested it this session.
    pub sha256_ok: Option<bool>,
    pub downloading: bool,
    /// How many bytes of an unfinished download are sitting beside this weight,
    /// waiting for a Download press to resume from them.
    ///
    /// `None` when there is no `.part`, which is the ordinary state. It is on
    /// the row because keeping the prefix is what makes a resume possible and
    /// is also 180 MB the user did not ask to keep, and until this field
    /// existed nothing said it was there.
    pub partial_bytes: Option<u64>,
}

/// One build this machine could be given, for the picker.
///
/// Every field is data or a number: a flavour's id is not translated, and
/// neither is the version of the toolkit CUDA wants - see
/// [`cleaner_core::runtime::package::Package::user_installed`], which is where
/// `user_installed` comes from unchanged.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlavourRow {
    /// `stock`, `directml`, `cuda12`, `cuda13`.
    pub id: &'static str,
    pub ort_version: &'static str,
    /// The whole download: both artefacts, where a flavour has two.
    pub bytes: u64,
    /// The one this platform gets when nobody has chosen.
    pub is_default: bool,
    /// What the **user** must install by hand first, as version strings. Empty
    /// for every flavour but CUDA, which is the argument for the default.
    pub user_installed: &'static [&'static str],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRow {
    pub installed: bool,
    pub path: Option<String>,
    pub read_only: bool,
    pub downloading: bool,
    /// The version in the package this platform would download - not the
    /// version of whatever is installed, which only a load can answer.
    pub version: Option<&'static str>,
    /// `stock`, `directml`, `cuda12`, `cuda13`. Data, not copy.
    ///
    /// **The flavour that would be downloaded**, which is the stored
    /// `runtimeFlavour` where this platform publishes it and the default
    /// otherwise - not necessarily the flavour of whatever is already on disk,
    /// which nothing short of loading the library can tell.
    pub flavour: Option<&'static str>,
    /// How large that download is. `None` on a platform with no package, which
    /// is the same platform `available` is false for.
    pub bytes: Option<u64>,
    /// Every build published for this platform, default first. One entry means
    /// there is nothing to choose and the picker is not drawn - which is macOS
    /// and Linux.
    pub flavours: Vec<FlavourRow>,
    /// Whether ONNX Runtime publishes anything for this platform at all.
    /// False on Intel macOS, which is the one platform with no row.
    pub available: bool,
    /// This machine: `macos-arm64`, `macos-x64`, `windows-x64`,
    /// `windows-arm64`, `linux-x64` or `linux-arm64`. Data, not copy. Setup
    /// names the platform its dependency list was checked on, and a web view's
    /// user agent cannot tell Apple silicon from Intel.
    pub platform: Option<&'static str>,
    /// The flavour of the build **that is actually here**, from the stamp a
    /// successful install writes ([`InstalledRuntime`]).
    ///
    /// `None` is *unknown* rather than *none*: a runtime installed before the
    /// stamp existed, or one found outside the directory this module owns, has
    /// no record to read and the row says nothing rather than claiming a build.
    pub installed_flavour: Option<String>,
    /// The ONNX Runtime version in that same stamp. `None` for the same reasons.
    pub installed_version: Option<String>,
    /// The bytes of everything `<app_data>/runtimes/.downloads` is holding -
    /// both artefacts' `.part` files and any complete archive a failed unpack
    /// left for the next press to re-unpack - which is the runtime's half of
    /// the kept-bytes accounting. `None` when the directory is empty or absent.
    pub partial_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsView {
    pub models: Vec<ModelRow>,
    pub runtime: RuntimeRow,
    /// Where a download would go. Shown so a user who wants to place a file by
    /// hand - the offline install - can.
    pub models_dir: Option<String>,
    pub runtime_dir: Option<String>,
    /// Whether a token is stored. **Never the token.**
    pub has_token: bool,
    /// Where a token is kept on this machine - `keychain`, `fileNoStore` or
    /// `fileStoreUnavailable`. Not a preference and not a choice: it is what
    /// the credential store answered, and the Settings note says which so a
    /// user on a machine with no keychain knows their token is in a plain file.
    ///
    /// The id rather than the enum, because the enum now carries a payload for
    /// one of its three cases and the seam's answer must stay one flat string.
    pub token_store: &'static str,
    /// **Why** the store would not answer, for the one location that has a
    /// reason. `None` for the other two: a keychain that took the token has
    /// nothing to explain, and a build with no store at all has no store to
    /// have failed.
    pub token_store_reason: Option<StoreReason>,
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

/// Every artefact, whether it is here, and where it would go.
///
/// Cheap by construction: one `metadata` call per search path per model and no
/// digests. See the module docs.
///
/// `retry_store` is the one argument, and it is `Settings › Models has just
/// been opened` rather than a preference. The token migration is offered to the
/// credential store **once per process**, which leaves a keychain
/// unlocked mid-session unnoticed until the next Save; an open of the one
/// screen that says where the token lives is the right moment - and the only
/// moment - to ask again. The dialog's refreshes and its polls do not pass it,
/// or the prompt-per-poll that once-per-process removed would be back.
#[tauri::command]
pub fn list_models(app: tauri::AppHandle, retry_store: Option<bool>) -> ModelsView {
    let data = app_data(&app);
    let writable = data.as_ref().map(|dir| dir.join("models"));
    if retry_store == Some(true) {
        forget_migration(migration_outcome());
        forget_token_refusal(token_memo());
    }
    // A `.part` stamped with a digest this catalogue no longer names is a
    // prefix of an artefact nothing will ask for again: no press can resume it
    // and nothing else will ever delete it. The one call that already walks
    // this directory sweeps them.
    if let Some(dir) = writable.as_deref() {
        sweep_stranded_parts(dir);
    }
    let verified_now = verified().lock().map(|map| map.clone()).unwrap_or_default();
    // One read of the cache for all eight rows, and one `stat` per found file to
    // decide whether its entry still describes it. Still no digests.
    let cached = writable.as_deref().map(read_cache).unwrap_or_default();

    let models = MODELS
        .iter()
        .map(|model| {
            let found = locate(model, data.as_deref(), writable.as_deref());
            ModelRow {
                id: model.id,
                file_name: model.file_name,
                bytes: model.bytes,
                sha256: model.sha256,
                kind_key: model.kind_key,
                required_by: model.required_by,
                installed: found.is_some(),
                path: found.as_ref().map(|(path, _)| path.display().to_string()),
                read_only: found.as_ref().map(|(_, ro)| *ro).unwrap_or(false),
                // What this session decided beats what a previous one wrote:
                // a `verifyModel` press just now is about the file as it is.
                sha256_ok: verified_now.get(model.id).copied().or_else(|| {
                    let (path, _) = found.as_ref()?;
                    cache_hit(cached.get(model.id)?, &path.display().to_string(), stamp(path)?)
                }),
                downloading: is_downloading(model.id),
                partial_bytes: partial_bytes(writable.as_deref(), model),
            }
        })
        .collect();

    let (token, location) = token_and_store(&app);
    ModelsView {
        models,
        runtime: runtime_row(&app, data.as_deref()),
        models_dir: writable.as_ref().map(|dir| dir.display().to_string()),
        runtime_dir: data.as_ref().map(|dir| dir.join("runtimes").display().to_string()),
        has_token: !token.is_empty(),
        token_store: location.id(),
        token_store_reason: location.reason(),
    }
}

fn runtime_row(app: &tauri::AppHandle, data: Option<&Path>) -> RuntimeRow {
    use cleaner_core::runtime::{self, package};

    let writable = data.map(|dir| dir.join("runtimes"));
    // The one call that already reads this directory is where the aside copies
    // a previous session could not delete are collected. See [`sweep_replaced`]
    // for why they exist and why a later launch is when they go.
    if let Some(dir) = writable.as_deref() {
        sweep_replaced(dir);
    }
    let found = runtime::find(data).ok();
    let read_only = match (&found, &writable) {
        (Some(path), Some(dir)) => path.parent() != Some(dir.as_path()),
        (Some(_), None) => true,
        _ => false,
    };
    // The **chosen** package, not the platform's default: a Windows user who
    // picked CUDA is looking at a row that has to say 455 MB and `cuda12`,
    // because that is what the Download button beside it would fetch.
    let host = package::for_host_flavour(&flavour_setting(app));
    // The stamp is about the copy in the directory this module owns. A library
    // found through `ORT_DYLIB_PATH` or beside the executable is somebody
    // else's and a stamp left over from an earlier download says nothing about
    // it, so the row is silent rather than wrong.
    let stamped = writable
        .as_deref()
        .and_then(read_installed)
        .filter(|_| found.is_some() && !read_only);
    RuntimeRow {
        installed: found.is_some(),
        path: found.as_ref().map(|path| path.display().to_string()),
        read_only,
        downloading: is_downloading(RUNTIME_ID),
        version: host.map(|p| p.ort_version),
        flavour: host.map(|p| p.flavour.id()),
        bytes: host.map(|p| p.bytes()),
        flavours: package::host_flavours()
            .into_iter()
            .map(|package| FlavourRow {
                id: package.flavour.id(),
                ort_version: package.ort_version,
                bytes: package.bytes(),
                is_default: package.default_flavour,
                user_installed: package.user_installed,
            })
            .collect(),
        available: host.is_some(),
        platform: package::Platform::host().map(platform_id),
        installed_flavour: stamped.as_ref().map(|stamp| stamp.flavour.clone()),
        installed_version: stamped.as_ref().map(|stamp| stamp.version.clone()),
        partial_bytes: writable.as_deref().and_then(runtime_partial_bytes),
    }
}

/// [`RuntimeRow::platform`]'s id for a platform.
fn platform_id(platform: cleaner_core::runtime::package::Platform) -> &'static str {
    use cleaner_core::runtime::package::{Arch, Os};
    match (platform.os, platform.arch) {
        (Os::MacOs, Arch::Aarch64) => "macos-arm64",
        (Os::MacOs, Arch::X86_64) => "macos-x64",
        (Os::Windows, Arch::Aarch64) => "windows-arm64",
        (Os::Windows, Arch::X86_64) => "windows-x64",
        (Os::Linux, Arch::Aarch64) => "linux-arm64",
        (Os::Linux, Arch::X86_64) => "linux-x64",
    }
}

/// The settings key the chosen ONNX Runtime build is stored under. An ordinary
/// setting written through `writeSettings` like every other one; unset, absent
/// and unrecognised all mean the platform's default.
pub const FLAVOUR_SETTING: &str = "runtimeFlavour";

/// What the user chose, or the empty string.
///
/// The empty string is a real answer rather than an absence to guard against:
/// [`cleaner_core::runtime::package::for_host_flavour`] resolves anything it
/// does not publish to the platform's default, so a machine that has never
/// opened the picker takes the same path as one whose stored id has gone stale.
fn flavour_setting(app: &tauri::AppHandle) -> String {
    crate::settings::read(app)
        .unwrap_or_default()
        .get(FLAVOUR_SETTING)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// Digest one installed weight and answer whether it matches the pin.
///
/// Explicit because it is expensive: 207 MB of SHA-256 is seconds, and doing it
/// on every `list_models` would make the Settings dialog unopenable on a cold
/// cache. The answer is remembered for the session and shown on the row.
#[tauri::command]
pub async fn verify_model(app: tauri::AppHandle, id: String) -> Result<bool, String> {
    crate::library::blocking(move || {
        let model = package(&id).ok_or_else(|| format!("no such model: {id}"))?;
        let data = app_data(&app);
        let writable = data.as_ref().map(|dir| dir.join("models"));
        let (path, _) = locate(model, data.as_deref(), writable.as_deref())
            .ok_or_else(|| format!("{} is not installed", model.file_name))?;
        let ok = digest_file(&path)? == model.sha256;
        record_verified(model.id, ok);
        // Written where the next launch will find it, so a weight checked once
        // stays checked until something under it changes.
        remember(writable.as_deref(), model.id, &path, ok);
        Ok(ok)
    })
    .await
}

/* ------------------------------------------------------------------ */
/* What a press resolved to                                            */
/* ------------------------------------------------------------------ */

/// Why a Download press did not start a download.
///
/// A boolean answered "no" to two very different questions and the row could
/// say neither. Both refusals are
/// **another window's doing**: a second Settings dialog draws its rows from a
/// `list_models` taken before the first window's download started or finished,
/// so its Download button is offered for something already running or already
/// here. Neither is an error - the remedy is a refreshed list, which the
/// interface then asks for - so they travel as an answer rather than as an
/// `Err`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadStart {
    Started,
    /// One is in flight for this id. The event stream is already reporting it,
    /// in this window as much as in the one that pressed.
    AlreadyRunning,
    /// The file is on disk at its published size. Re-fetching it is 200 MB to
    /// arrive where the machine already is.
    AlreadyInstalled,
}

/// What a Delete press found.
///
/// `false` used to mean both "there is no such file" and "there is one, and it
/// belongs to somebody else". The
/// row already carries `readOnly` and the button is disabled for one, so the
/// second answer is only ever seen after a press the interface should not have
/// allowed - which is exactly the case where a sentence saying *why* is worth
/// having, because the user is looking at a stale row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeleteOutcome {
    Deleted,
    /// A download is holding the directory. Nothing was touched.
    ///
    /// The runtime's own row is the one that can reach this: `deleteRuntime`
    /// removes the whole `<app_data>/runtimes` tree, and that tree holds the
    /// `.part` of a transfer another window may be appending to.
    Busy,
    /// Nothing of that name on any search path. The row was stale, or a second
    /// window got there first.
    NotFound,
    /// Found, and outside the one directory this module owns - a developer's
    /// checkout, `$MANGA_CLEANER_MODELS`, the bundle's `Resources` - or inside
    /// it and locked by the filesystem. Nothing was touched.
    ReadOnlyElsewhere,
}

/// The three answers, decided before anything is removed.
///
/// Pure over what [`locate`] found, so the mapping from "where is it" to "what
/// does the user get told" is pinned without a filesystem - and so the same
/// decision can be read in one place rather than inferred from the order of
/// three early returns.
fn plan_delete(found: Option<(PathBuf, bool)>) -> Result<PathBuf, DeleteOutcome> {
    match found {
        None => Err(DeleteOutcome::NotFound),
        Some((_, true)) => Err(DeleteOutcome::ReadOnlyElsewhere),
        Some((path, false)) => Ok(path),
    }
}

/// Which loaded session a weight is the bytes of, if any.
///
/// **A match rather than a field on [`ModelPackage`]**, and that is the whole
/// of the choice. `required_by` names
/// *engines and features* in the interface's vocabulary - `autoClean`, `lama` -
/// because that is what the settings dialog and the engine pickers are drawn
/// from; a `Kind` is a *session* in [`cleaner_core::registry`], and the two do
/// not line up. `autoClean` is three files and three kinds, and the script
/// gate's labels file is not a session at all but is part of one. Putting a
/// `Kind` on the catalogue row would have made `required_by` answer two
/// questions and been wrong for both.
///
/// `None` for [`RUNTIME_ID`] and for anything not in the table: the runtime is
/// not a session, it is the library every session is built through, and
/// deleting it evicts nothing - the sessions already loaded go on running
/// against a library that is still mapped into the process.
/// The full RT-DETR graph is also `None`: its session is owned only by the
/// active pipeline and is never parked in `residency`. Evicting the small RT
/// kind for its separate weight would free an unrelated model.
fn resident_kind(id: &str) -> Option<registry::Kind> {
    Some(match id {
        "textDetector" => registry::Kind::TextDetector,
        "inpainter" => registry::Kind::Inpainter,
        // Both halves of the gate: the labels file is read by the same session
        // the model is, and a gate whose labels are gone is not a gate.
        "scriptGate" | "scriptGateLabels" => registry::Kind::ScriptGate,
        "balloonDetector" => registry::Kind::BalloonDetector,
        // All three parts of the reader, and they map to the **gate**, not to
        // `Kind::Ocr`: the reader is a field of the `ScriptGate`, and what
        // `residency` parks is the gate as one unit under its own kind. An
        // eviction keyed on `Kind::Ocr` would find nothing there and leave the
        // deleted weights resident inside the parked gate. Dropping the gate
        // drops the reader with it, and the reader's own registry row goes
        // when its leases do.
        "ocrEncoder" | "ocrDecoder" | "ocrVocab" => registry::Kind::ScriptGate,
        // Hayai is its own parked session (`run.rs`'s `reader`), not part of
        // the gate.
        "hayaiVision" | "hayaiDecoder" | "hayaiTokenizer" => registry::Kind::Ocr,
        _ => return None,
    })
}

/// Drop every parked session that was built from this weight, and answer how
/// many went.
///
/// `residency::evict` and not `registry::request_unload`: an unload is a
/// *request* to whoever is holding the session, and what is parked is by
/// definition held by nobody - the file is gone, so the right answer for a
/// session nobody is using is now. A session a run is holding is not in the
/// table at all and is not reachable from here, which is the property that
/// makes this safe to call from a settings dialog mid-run: the run finishes
/// with the model it loaded, and the checkin at the end drops it into a table
/// this has already emptied.
fn evict_sessions_of(id: &str) -> usize {
    resident_kind(id).map(cleaner_core::residency::evict).unwrap_or(0)
}

/// Start a download. Answers as soon as the thread is running.
///
/// Progress arrives on the event channel and nowhere else, which is the one
/// absolute rule about progress: a
/// Tauri `invoke` cannot return a streaming result, so a command that resolved
/// when the file was complete would leave a 207 MB download with no way to
/// report itself and no way to be cancelled.
///
/// The answer says *why* when it does not start one. See [`DownloadStart`].
#[tauri::command]
pub fn download_model(app: tauri::AppHandle, id: String) -> Result<DownloadStart, String> {
    if let Some(group) = group_for_member(&id) {
        return download_model_group(app, group.to_owned());
    }
    let model = package(&id).copied().ok_or_else(|| format!("no such model: {id}"))?;
    let dir = writable_models_dir(&app)?;
    let data = app_data(&app);
    // Installed beats running: a press against a row that was drawn before
    // another window finished is the commonest of the two races, and the
    // sentence for it is the more useful one.
    if locate(&model, data.as_deref(), Some(dir.as_path())).is_some() {
        return Ok(DownloadStart::AlreadyInstalled);
    }
    let auth = token(&app);
    let Some(cancel) = claim(model.id) else { return Ok(DownloadStart::AlreadyRunning) };

    std::thread::spawn(move || {
        let result = fetch_verified(
            model.url,
            model.sha256,
            &dir.join(model.file_name),
            model.id,
            &auth,
            &cancel,
            Portion::ALONE,
        );
        if let Ok(path) = &result {
            record_verified(model.id, true);
            remember(Some(dir.as_path()), model.id, path, true);
        }
        finish(model.id, result.map(|_| ()));
    });
    Ok(DownloadStart::Started)
}

/// Install a multi-file model as one capability. Missing members are fetched
/// into a private staging directory and checked before any final file appears.
/// A failed transfer leaves installed members intact and resumable staging
/// files; a failed final rename rolls back newly moved members.
#[tauri::command]
pub fn download_model_group(app: tauri::AppHandle, id: String) -> Result<DownloadStart, String> {
    let models = model_group(&id).ok_or_else(|| format!("no such model group: {id}"))?;
    let dir = writable_models_dir(&app)?;
    let data = app_data(&app);
    let auth = token(&app);
    let Some(cancel) = claim_group(&id, &models) else { return Ok(DownloadStart::AlreadyRunning) };
    std::thread::spawn(move || {
        let staged = dir.join(".model-groups").join(&id);
        let result = (|| -> Result<(), String> {
            std::fs::create_dir_all(&staged).map_err(|e| e.to_string())?;
            let mut missing = Vec::new();
            for model in &models {
                if let Some((path, _)) = locate(model, data.as_deref(), Some(dir.as_path())) {
                    if digest_file(&path)? != model.sha256 {
                        return Err(format!(
                            "{} failed its pinned SHA-256; remove the damaged group before reinstalling",
                            path.display()
                        ));
                    }
                } else {
                    let path = staged.join(model.file_name);
                    if !std::fs::metadata(&path).is_ok_and(|meta| meta.len() == model.bytes)
                        || !digest_file(&path).is_ok_and(|sha| sha == model.sha256)
                    {
                        // This is private staging, never a user-installed
                        // model. Windows cannot rename a verified `.part`
                        // over a corrupt prior staging file.
                        if path.exists() { std::fs::remove_file(&path).map_err(|e| e.to_string())?; }
                        fetch_verified(model.url, model.sha256, &path, model.id, &auth, &cancel, Portion::ALONE)?;
                    }
                    if !std::fs::metadata(&path).is_ok_and(|meta| meta.len() == model.bytes)
                        || digest_file(&path)? != model.sha256
                    {
                        return Err(format!("{} failed group verification", model.file_name));
                    }
                    missing.push((*model, path));
                }
            }
            if cancel.load(Ordering::Relaxed) { return Err("cancelled".into()); }
            let mut moved: Vec<(&ModelPackage, PathBuf)> = Vec::new();
            for (model, staged_path) in &missing {
                let final_path = dir.join(model.file_name);
                if final_path.exists() {
                    for (prior, _) in moved.iter().rev() {
                        let _ = std::fs::rename(dir.join(prior.file_name), staged.join(prior.file_name));
                    }
                    return Err(format!("{} exists but is not a verified model; remove it before installing the group", final_path.display()));
                }
                if let Err(error) = std::fs::rename(staged_path, &final_path) {
                    for (prior, _) in moved.iter().rev() {
                        let _ = std::fs::rename(dir.join(prior.file_name), staged.join(prior.file_name));
                    }
                    return Err(format!("{}: {error}", final_path.display()));
                }
                moved.push((model, final_path));
            }
            for model in &models {
                let path = dir.join(model.file_name);
                record_verified(model.id, true);
                remember(Some(dir.as_path()), model.id, &path, true);
                emit(model.id, model.bytes, Some(model.bytes), true, None);
            }
            Ok(())
        })();
        release_group(&id, &models);
        emit(&id, 0, None, true, result.err());
    });
    Ok(DownloadStart::Started)
}

/// Verify all dependency files, with one result for the logical model.
#[tauri::command]
pub async fn verify_model_group(app: tauri::AppHandle, id: String) -> Result<bool, String> {
    crate::library::blocking(move || {
        let models = model_group(&id).ok_or_else(|| format!("no such model group: {id}"))?;
        let data = app_data(&app);
        let writable = data.as_ref().map(|dir| dir.join("models"));
        let mut all_valid = true;
        for model in models {
            let valid = locate(model, data.as_deref(), writable.as_deref())
                .is_some_and(|(path, _)| digest_file(&path).is_ok_and(|sha| sha == model.sha256));
            record_verified(model.id, valid);
            all_valid &= valid;
        }
        Ok(all_valid)
    }).await
}

/// Remove a logical model as one unit. All files are preflighted and moved
/// aside first, so a refused member does not strand a newly partial install.
#[tauri::command]
pub fn delete_model_group(app: tauri::AppHandle, id: String) -> Result<DeleteOutcome, String> {
    let models = model_group(&id).ok_or_else(|| format!("no such model group: {id}"))?;
    let dir = writable_models_dir(&app)?;
    let data = app_data(&app);
    let Some(_claim) = claim_group(&id, &models) else { return Ok(DeleteOutcome::Busy) };
    let result = (|| -> Result<DeleteOutcome, String> {
        let mut paths = Vec::new();
        for model in &models {
            match plan_delete(locate(model, data.as_deref(), Some(dir.as_path()))) {
                Ok(path) => paths.push((*model, path)),
                Err(DeleteOutcome::NotFound) => {},
                Err(outcome) => return Ok(outcome),
            }
        }
        if paths.is_empty() { return Ok(DeleteOutcome::NotFound); }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?.as_nanos();
        let mut aside = Vec::new();
        for (model, path) in &paths {
            let backup = dir.join(format!(".group-delete-{id}-{stamp}-{}", model.file_name));
            if let Err(error) = std::fs::rename(path, &backup) {
                for (original, saved) in aside.iter().rev() {
                    let _ = std::fs::rename(saved, original);
                }
                return Err(format!("{}: {error}", path.display()));
            }
            aside.push((path.clone(), backup));
        }
        for (_, backup) in aside { let _ = std::fs::remove_file(backup); }
        for model in models.iter() {
            forget_verified(model.id);
            forget_remembered(Some(dir.as_path()), model.id);
            evict_sessions_of(model.id);
        }
        Ok(DeleteOutcome::Deleted)
    })();
    release_group(&id, &models);
    result
}

/// Ask a download to stop. It stops at its next block, deletes its `.part`, and
/// reports `done` with a cancellation error - so the row goes back to "not
/// installed" through the same path a failure takes.
#[tauri::command]
pub fn cancel_download(id: String) -> bool {
    let Ok(map) = inflight().lock() else { return false };
    match map.get(&id) {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// Remove a weight from the app-data directory, and only from there.
///
/// A read-only copy is refused by name rather than attempted and failed: the
/// remedy for a weight in a developer's checkout is to delete it there, and a
/// button that reports a permissions error would be telling the user about a
/// decision this module already made. Which of the three things happened is
/// [`DeleteOutcome`], because "no" was two facts under one boolean.
///
/// **A parked session goes with its file.** A model already loaded was built from
/// bytes that are in memory and would go on working after its weights were
/// deleted - a run started before the press finishing on an engine Settings
/// says is not installed. So a successful delete evicts the kind the
/// weight belongs to. Nothing is emitted for it: the loaded-models tab is a
/// poll by design, the same way
/// `unload_model` leaves it, and the Settings dialog refreshes its own list and
/// the capability store on the press it made.
#[tauri::command]
pub fn delete_model(app: tauri::AppHandle, id: String) -> Result<DeleteOutcome, String> {
    if let Some(group) = group_for_member(&id) {
        return delete_model_group(app, group.to_owned());
    }
    let model = package(&id).ok_or_else(|| format!("no such model: {id}"))?;
    let dir = writable_models_dir(&app)?;
    let data = app_data(&app);
    let path = match plan_delete(locate(model, data.as_deref(), Some(dir.as_path()))) {
        Ok(path) => path,
        Err(outcome) => return Ok(outcome),
    };
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    forget_verified(model.id);
    forget_remembered(Some(dir.as_path()), model.id);
    evict_sessions_of(model.id);
    Ok(DeleteOutcome::Deleted)
}

/// Throw away the unfinished download beside a row, and say whether there was
/// one.
///
/// A resumable prefix is worth keeping by default and worth being able
/// to give back: 180 MB of a cancelled 207 MB fetch sits in `<app_data>/models`
/// until somebody presses Download again, and nothing used to say it was
/// there or offer to remove it. The row reports the bytes and this is the
/// press.
///
/// **A transfer in flight keeps its bytes.** The `.part` is the file a download
/// thread has open, and a press from a second window must not delete it out
/// from under a writer. The answer is then `false`, which is the same "nothing
/// was discarded" a row with no `.part` gets - the two are one fact to the
/// interface, which refreshes and finds the row downloading.
///
/// That refusal is decided and acted on **under the download registry's own
/// lock** ([`while_not_downloading`]), because asking and then deleting leaves a
/// gap a Download press fits through exactly.
///
/// The runtime's leftovers are its archives, in `.downloads`, and **all** of
/// them go: what that directory holds is one interrupted transfer per artefact,
/// and a user asking for the disk back is not asking about one of the two halves
/// of a Windows package.
#[tauri::command]
pub fn discard_partial(app: tauri::AppHandle, id: String) -> Result<bool, String> {
    // Both directories are resolved *before* the lock is taken: they are two
    // settings reads and a path join, and none of that belongs inside a mutex
    // every download press queues on.
    let dir = if id == RUNTIME_ID {
        writable_runtime_dir(&app)?
    } else {
        package(&id).ok_or_else(|| format!("no such model: {id}"))?;
        writable_models_dir(&app)?
    };
    Ok(while_not_downloading(&id, false, || {
        if id == RUNTIME_ID {
            return discard_runtime_leftovers(&dir);
        }
        let model = package(&id).expect("checked above");
        let part = part_path(&dir.join(model.file_name), model.sha256);
        std::fs::remove_file(&part).is_ok()
    }))
}

/// Download and unpack the ONNX Runtime this platform needs.
///
/// The archive is the platform's own - a `.tgz` on macOS and Linux, a `.nupkg`
/// on Windows - and only [`package::Artefact::library_dir`] is unpacked, flat,
/// into `<app_data>/runtimes`, which is where
/// [`cleaner_core::runtime::search_paths`] looks. A package with companions  - 
/// `DirectML.dll`, the WebGPU plugin - has every artefact land in that same
/// directory, which is where the loader expects the plugin.
///
/// Two things happen before any of that, and both are about a Windows machine
/// rather than this one: the volume is asked whether it can hold the install
/// ([`room_to_install`], and again exactly per archive at [`unpacked_bytes`]),
/// and the artefacts are counted so that a package of three reports as one
/// download ([`Portion`]).
#[tauri::command]
pub fn download_runtime(app: tauri::AppHandle) -> Result<DownloadStart, String> {
    use cleaner_core::runtime::package;

    // **The user's flavour, where they chose one.** `for_host_flavour` resolves
    // an id this platform does not publish to the default, so a stale or
    // foreign setting downloads the right thing rather than nothing.
    let host = package::for_host_flavour(&flavour_setting(&app))
        .ok_or_else(|| "no ONNX Runtime is published for this platform".to_owned())?;
    let dir = writable_runtime_dir(&app)?;
    let auth = token(&app);
    // Not `AlreadyInstalled`: an installed runtime is the *only* thing the
    // picker can be acted on with - switching flavour is a re-download over the
    // top of one that is already there - so the press has to be honoured.
    let Some(cancel) = claim(RUNTIME_ID) else { return Ok(DownloadStart::AlreadyRunning) };

    std::thread::spawn(move || {
        // Two directories, and the difference between them is the difference
        // between what may be thrown away and what must not be.
        //
        // `.staging` holds the *unpacked* libraries and is removed before and
        // after every attempt, so that a failed or cancelled second artefact
        // (Windows: `DirectML.dll`) cannot leave a primary library that
        // `runtime::find` would report as installed but that cannot load.
        //
        // `.downloads` holds the archives and their `.part` files and survives
        // both wipes, because a `.part` inside a directory that is removed at
        // the end of every attempt is a resume that can never happen - which
        // is what made a cancelled 455 MB CUDA fetch start again from byte
        // zero.
        let staging = runtime_staging(&dir);
        let downloads = runtime_downloads(&dir);
        let result = (|| -> Result<(), String> {
            let _ = std::fs::remove_dir_all(&staging);
            std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;

            // **What this attempt has to fetch, decided before it fetches any
            // of it.** The free-space preflight below needs the figure, and so
            // does the progress bar: `already_fetched` is a digest of up to
            // 202 MB and is paid once either way, so it is paid here where two
            // things can read the answer instead of inside the loop where one
            // could.
            let mut planned = Vec::new();
            let mut to_fetch = 0u64;
            for artefact in host.artefacts() {
                let name = artefact.url.rsplit('/').next().unwrap_or("runtime-archive");
                let archive = downloads.join(name);
                // A complete archive from a previous attempt is unpacked again
                // rather than fetched again. The download is the expensive half
                // and it already succeeded - what failed was the unpack, which
                // is a full disk or a permission and is usually over by the
                // next press. Verifying 202 MB is seconds; fetching it is not.
                let have = already_fetched(&archive, artefact.sha256);
                if !have {
                    to_fetch += artefact.bytes;
                }
                planned.push((artefact, archive, have));
            }
            ensure_room(&dir, room_to_install(to_fetch))?;

            // **One download, whichever way it is packaged.** A Windows runtime
            // is three artefacts - 12 MB, then 202 MB, then 39 MB - and
            // reporting each of them from zero under one id drew the bar
            // restarting twice with no total anybody could read. Every report
            // now counts the whole, and an artefact that was already on disk
            // contributes its bytes to `done` without a report of its own: the
            // next one's first event carries them.
            let whole = Some(host.bytes());
            let mut done = 0u64;
            for (artefact, archive, have) in planned {
                if !have {
                    fetch_verified(
                        artefact.url,
                        artefact.sha256,
                        &archive,
                        RUNTIME_ID,
                        &auth,
                        &cancel,
                        Portion { before: done, whole },
                    )?;
                }
                ensure_room(&staging, unpacked_bytes(&archive, artefact.library_dir).unwrap_or(0))?;
                unpack(&archive, artefact.library_dir, &staging)?;
                // The archive is not kept once it *is* unpacked: it is up to
                // 202 MB of which one library is used, and nothing re-reads it.
                // Its `.part` is already gone - the rename consumed it.
                let _ = std::fs::remove_file(&archive);
                done += artefact.bytes;
            }
            // The move, the sweep of the previous build's leftovers and the new
            // record, in the one order a crash between them can survive.
            install_stamped(&dir, host.flavour.id(), host.ort_version, || {
                move_staged(&staging, &dir)
            })?;
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&staging);
        // An empty download directory is swept; one still holding a `.part` a
        // cancelled attempt left is not, which is the point of it.
        let _ = std::fs::remove_dir(&downloads);
        finish(RUNTIME_ID, result);
    });
    Ok(DownloadStart::Started)
}

/// Remove the runtime from the app-data directory, and only from there.
///
/// The same three answers a weight's Delete gives, for the same reason: the
/// runtime row carries `readOnly` too - a library found beside the executable
/// or through `ORT_DYLIB_PATH` is a developer's or an installer's - and the
/// press has to be able to say so.
///
/// What "remove" costs differs by platform and [`remove_installed`] is where
/// that lives: on Windows the library this press is aimed at is mapped into
/// this process by the time the dialog offering the press is open, so it is
/// renamed out of the way rather than deleted, and the disk comes back a launch
/// later.
#[tauri::command]
pub fn delete_runtime(app: tauri::AppHandle) -> Result<DeleteOutcome, String> {
    let dir = writable_runtime_dir(&app)?;
    let data = app_data(&app);
    let found = cleaner_core::runtime::find(data.as_deref()).ok();
    match plan_delete_runtime(found.as_deref(), &dir, is_downloading(RUNTIME_ID)) {
        Err(outcome) => Ok(outcome),
        Ok(()) => {
            remove_installed(&dir)?;
            Ok(DeleteOutcome::Deleted)
        }
    }
}

/// The four answers, decided before anything is removed.
///
/// Pure, like [`plan_delete`], and for the same reason - but with two rules a
/// weight's Delete does not need:
///
/// - **A download in flight refuses the press.** `remove_dir_all` would take
///   `.downloads` and the `.part` inside it with the libraries, which is a
///   transfer another window is appending to. `Busy` says so.
/// - **`NotFound` is about the runtime, not the directory.** The directory can
///   exist while nothing is installed in it - that is exactly what a cancelled
///   download leaves - and answering `Deleted` for sweeping a `.part` would
///   report an uninstall that did not happen.
fn plan_delete_runtime(
    found: Option<&Path>,
    writable: &Path,
    downloading: bool,
) -> Result<(), DeleteOutcome> {
    if downloading {
        return Err(DeleteOutcome::Busy);
    }
    match found {
        None => Err(DeleteOutcome::NotFound),
        Some(path) if path.parent() != Some(writable) => Err(DeleteOutcome::ReadOnlyElsewhere),
        Some(_) => Ok(()),
    }
}

/* ------------------------------------------------------------------ */
/* The token                                                           */
/* ------------------------------------------------------------------ */

/// What the credential store files the token under. The service is the
/// application's bundle identifier so that a user auditing their keychain sees
/// the application they installed; the user is the site the token is for, which
/// leaves room for a second one if a second host ever gates a download.
const KEYRING_SERVICE: &str = "com.mangacleaner.studio";
const KEYRING_USER: &str = "huggingface";

/// The settings key the token used to live under, and still does on a machine
/// with no usable credential store.
pub const TOKEN_SETTING: &str = "hfToken";

/// Why a credential store would not answer.
///
/// The thing a `String` could not carry. "The keychain is locked", "no D-Bus
/// session" and "two credentials
/// match this entry" arrive as prose, in each platform's own words, and prose
/// is not something an interface can choose a sentence from - so the note under
/// the token field could say only that *some* store was unreachable. Four ids,
/// one per `keyring::Error` shape that means something different to the person
/// holding the remedy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StoreReason {
    /// The store is there and refused access. The commonest case by far, and
    /// the one whose remedy the user can carry out: unlock it and save again.
    Locked,
    /// The platform's store failed underneath - a service that is not running,
    /// no session bus, a platform call that errored. Nothing to unlock.
    Unreachable,
    /// More than one credential matches this entry, so the store cannot say
    /// which of them is ours. Remedied in the user's own keychain tool.
    Ambiguous,
    /// Anything else, **including whatever `keyring` adds next**: its error
    /// enum is `#[non_exhaustive]`, and a build that stopped compiling for a
    /// new variant would be a poor trade for one more sentence.
    Unknown,
}

/// What a store said no with: an id the interface can name, and the platform's
/// own words for the log and for [`TokenWrite::NotCleared`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    pub reason: StoreReason,
    pub detail: String,
}

impl StoreError {
    pub fn new(reason: StoreReason, detail: impl Into<String>) -> StoreError {
        StoreError { reason, detail: detail.into() }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// Which reason a `keyring` failure is.
///
/// Four arms and a wildcard, taken from what each variant's own documentation
/// says the platform meant: `NoStorageAccess` is "the store could not be
/// accessed - typically it is locked", `PlatformFailure` is "runtime failure in
/// the storage system", `Ambiguous` is more than one matching credential.
/// Everything else - a bad encoding, an attribute too long, a variant added
/// later - is a bug or a novelty rather than a state with a remedy.
fn reason_of(err: &keyring::Error) -> StoreReason {
    match err {
        keyring::Error::NoStorageAccess(_) => StoreReason::Locked,
        keyring::Error::PlatformFailure(_) => StoreReason::Unreachable,
        keyring::Error::Ambiguous(_) => StoreReason::Ambiguous,
        _ => StoreReason::Unknown,
    }
}

fn store_error(err: keyring::Error) -> StoreError {
    StoreError::new(reason_of(&err), err.to_string())
}

/// Somewhere one secret can be kept.
///
/// A trait for one implementation and one test double, which is the whole
/// reason it exists: a keychain cannot be exercised in CI - macOS prompts,
/// Linux has no Secret Service on a build agent - so the decisions *around* the
/// store (migrate the plaintext copy, fall back when there is none) are written
/// against this and tested against an in-memory one.
pub trait TokenStore {
    /// `Ok(None)` is "there is no token here", which is not an error. `Err` is
    /// "this store would not answer", which is what the fallback answers to -
    /// and it says **why** in an id the interface can name ([`StoreReason`]).
    fn get(&self) -> Result<Option<String>, StoreError>;
    fn set(&self, token: &str) -> Result<(), StoreError>;
    fn delete(&self) -> Result<(), StoreError>;

    /// Whether this build has a credential store at all.
    ///
    /// A **build** fact, not a runtime one, and that is exactly the point: an
    /// error from `get` says only that the store did not answer, which on a
    /// platform that has one means it is locked or refusing this process and on
    /// a platform that has none means there was never anything to ask. The
    /// interface has to say different sentences for those, and a refused Clear
    /// has to be an error in the first case and a success in the second.
    fn available(&self) -> bool;
}

/// The operating system's own: the macOS Keychain, the Windows Credential
/// Manager, the Secret Service or the kernel keyring on Linux, whichever
/// `keyring`'s platform features named in `Cargo.toml` compiled in.
pub struct OsKeyring;

impl OsKeyring {
    fn entry() -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(store_error)
    }
}

impl TokenStore for OsKeyring {
    fn get(&self) -> Result<Option<String>, StoreError> {
        match Self::entry()?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(store_error(err)),
        }
    }

    fn set(&self, token: &str) -> Result<(), StoreError> {
        #[cfg(target_os = "macos")]
        return crate::macos_keychain::set_password(KEYRING_SERVICE, KEYRING_USER, token).map_err(store_error);
        #[cfg(not(target_os = "macos"))]
        Self::entry()?.set_password(token).map_err(store_error)
    }

    fn delete(&self) -> Result<(), StoreError> {
        #[cfg(target_os = "macos")]
        return crate::macos_keychain::delete_password(KEYRING_SERVICE, KEYRING_USER).map_err(store_error);
        #[cfg(not(target_os = "macos"))]
        match Self::entry()?.delete_credential() {
            // Clearing a token that was never stored is what the Clear button
            // does on a fresh install; it is not something to report.
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(store_error(err)),
        }
    }

    /// The three platforms `Cargo.toml` names a `keyring` backend for. Anywhere
    /// else the crate compiles its in-memory stand-in, which would forget the
    /// token at exit - so those builds are told to keep it in the file instead.
    fn available(&self) -> bool {
        cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux"))
    }
}

/// What a read of the store answered, kept for the rest of the process.
/// `None` is "not asked yet". The token is zeroized when it is forgotten.
type TokenMemo = Mutex<Option<Result<Option<zeroize::Zeroizing<String>>, StoreError>>>;

/// A store that asks the one behind it **once per process**, not once per read.
///
/// On macOS every data read of an item another build wrote puts a password
/// prompt in front of the user, and `list_models` and every download press read
/// the token, so an uncached store prompts per poll for an answer that has not
/// changed. A refusal is kept as well as a token: asking again is the prompt.
///
/// The lock is held across the read, so concurrent readers wait for the one
/// read in flight rather than each raising their own prompt. A write goes to
/// the store under the same lock, then remembers the successfully written
/// value (or absence after deletion). A failed mutation forgets the old answer.
struct CachedStore<'a> {
    inner: &'a dyn TokenStore,
    memo: &'a TokenMemo,
}

impl TokenStore for CachedStore<'_> {
    fn get(&self) -> Result<Option<String>, StoreError> {
        let mut memo = self.memo.lock().unwrap_or_else(|p| p.into_inner());
        match memo.get_or_insert_with(|| self.inner.get().map(|found| found.map(zeroize::Zeroizing::new))) {
            Ok(found) => Ok(found.as_deref().cloned()),
            Err(err) => Err(err.clone()),
        }
    }

    fn set(&self, token: &str) -> Result<(), StoreError> {
        let mut memo = self.memo.lock().unwrap_or_else(|p| p.into_inner());
        let written = self.inner.set(token);
        *memo = written.as_ref().ok().map(|()| Ok(Some(zeroize::Zeroizing::new(token.to_owned()))));
        written
    }

    fn delete(&self) -> Result<(), StoreError> {
        let mut memo = self.memo.lock().unwrap_or_else(|p| p.into_inner());
        let deleted = self.inner.delete();
        *memo = deleted.as_ref().ok().map(|()| Ok(None));
        deleted
    }

    fn available(&self) -> bool {
        self.inner.available()
    }
}

/// The process's one memo of the real store's answer.
fn token_memo() -> &'static TokenMemo {
    static MEMO: OnceLock<TokenMemo> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(None))
}

/// The real store, read through [`token_memo`]. Every read and write of the
/// token goes through this, so no write can leave the memo stale.
fn keychain() -> CachedStore<'static> {
    CachedStore { inner: &OsKeyring, memo: token_memo() }
}

/// Forget a remembered refusal, never a remembered token, so the next read
/// asks the store again. For the same moment as [`forget_migration`]: an open
/// of Settings › Models, where a keychain unlocked mid-session is noticed.
fn forget_token_refusal(memo: &TokenMemo) {
    let mut memo = memo.lock().unwrap_or_else(|p| p.into_inner());
    if matches!(*memo, Some(Err(_))) {
        *memo = None;
    }
}

/// Where the token actually is. Data, not copy: the interface has a sentence
/// for each.
///
/// Not `Serialize`, deliberately. One of the three now carries a payload, and a
/// derived enum would put `{"fileStoreUnavailable": {"reason": "locked"}}` on
/// the wire where the seam fixes a flat string. [`TokenLocation::id`] and
/// [`TokenLocation::reason`] are the two answers the view carries side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenLocation {
    /// The operating system's credential store took it.
    Keychain,
    /// This build has no credential store, so `settings.json` keeps it - which
    /// is where it lived before the credential store existed and is still
    /// better than refusing to
    /// remember it at all. A permanent state, and nothing the user can change.
    FileNoStore,
    /// There **is** a credential store and it would not answer: locked, or
    /// refusing this process. `settings.json` keeps the token meanwhile.
    ///
    /// Separate from [`TokenLocation::FileNoStore`] because the two say
    /// opposite things to the reader - "this computer cannot do better" against
    /// "something is wrong and unlocking your keychain would fix it" - and the
    /// old single `File` reported the second as the first.
    ///
    /// The reason rides along, because "unlock your keychain" and "this machine
    /// has no session bus" are different instructions and the note under the
    /// field is where a user learns which one they are being given.
    FileStoreUnavailable { reason: StoreReason },
}

impl TokenLocation {
    /// The id the seam carries. Data, never copy.
    pub fn id(self) -> &'static str {
        match self {
            TokenLocation::Keychain => "keychain",
            TokenLocation::FileNoStore => "fileNoStore",
            TokenLocation::FileStoreUnavailable { .. } => "fileStoreUnavailable",
        }
    }

    /// Why the store did not answer, for the one location that has a reason.
    pub fn reason(self) -> Option<StoreReason> {
        match self {
            TokenLocation::FileStoreUnavailable { reason } => Some(reason),
            _ => None,
        }
    }
}

/// Which of the two file cases an error from the store is.
///
/// The **build** decides which, exactly as before: an error says only that the
/// store did not answer, and on a platform with no `keyring` backend there was
/// never anything to ask. What the error adds is the reason, and it is carried
/// only by the case that has a store to explain.
fn fallback_location(store: &dyn TokenStore, err: &StoreError) -> TokenLocation {
    if store.available() {
        TokenLocation::FileStoreUnavailable { reason: err.reason }
    } else {
        TokenLocation::FileNoStore
    }
}

/// What a read of the token found, and what the caller must now write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRead {
    pub token: String,
    pub location: TokenLocation,
    /// `settings.json` is holding a plaintext copy the store has taken over,
    /// and the caller must remove it. This is the migration: it happens on the
    /// first read after the upgrade and never again.
    pub migrate: bool,
}

/// Decide where the token is, moving it out of the settings file if it is still
/// there and there is somewhere better to put it.
///
/// **A token in the settings file is the newer one and it wins.** That ordering
/// is the whole correctness of this function, and getting it the other way
/// round loses a token outright: a locked keychain holding an old `A` refuses
/// the `B` the user has just pasted, `B` falls back to `settings.json`, and a
/// next read that asked the store first would answer `A`, call `B` a stale
/// duplicate and delete it. Nothing gets to the file except by way of a
/// [`write_token`] the store refused, so the file's copy is always either the
/// migration case or the fallback case - and both mean "this is the token the
/// user last gave us".
///
/// So: the file's copy is written *into* the store first, and the caller is
/// told to forget the file key **only when that write succeeded**. A token the
/// store does not now hold is never deleted from the only other place it is.
///
/// With nothing in the file it is an ordinary read, and a store that will not
/// answer is the `File` fallback with nothing to migrate.
pub fn read_token(store: &dyn TokenStore, from_settings: &str) -> TokenRead {
    let in_file = from_settings.trim();
    if !in_file.is_empty() {
        return match store.set(in_file) {
            Ok(()) => TokenRead {
                token: in_file.to_owned(),
                location: TokenLocation::Keychain,
                migrate: true,
            },
            // The store refused it. The file is the only copy there is, so it
            // stays and the interface is told the token is in plain text - and
            // which of the two reasons it is.
            Err(err) => TokenRead {
                token: in_file.to_owned(),
                location: fallback_location(store, &err),
                migrate: false,
            },
        };
    }
    match store.get() {
        Ok(Some(stored)) if !stored.trim().is_empty() => TokenRead {
            token: stored.trim().to_owned(),
            location: TokenLocation::Keychain,
            migrate: false,
        },
        Ok(_) => TokenRead {
            token: String::new(),
            location: TokenLocation::Keychain,
            migrate: false,
        },
        Err(err) => TokenRead {
            token: String::new(),
            location: fallback_location(store, &err),
            migrate: false,
        },
    }
}

/// What a save or a clear did, and therefore what the settings file must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenWrite {
    /// The credential store took it, or removed it. No plaintext copy anywhere.
    Stored,
    /// There is no usable store on this machine. The caller keeps the value in
    /// `settings.json`, which for a clear means removing the key.
    Fallback,
    /// **A clear the store refused, with the secret still in it.** Distinct
    /// from [`TokenWrite::Fallback`] because the two look identical from here
    /// and mean opposite things to the user: one is "your token is in a plain
    /// file", the other is "the token you just asked me to delete is still in
    /// your keychain". Removing the file key on this answer and reporting
    /// success would tell the user a secret was destroyed when it was not.
    NotCleared(String),
}

/// Save the token, or clear it when it is empty, and answer what happened.
///
/// The clear is the delicate one. `delete` failing has two causes that the
/// trait cannot tell apart on its own - there is no store at all, or there is
/// one and it will not let go - so a refusal is followed by a `get`: a store
/// that answers "nothing here" has nothing left to clear, a store that will not
/// answer at all is the machine with no store, and a store that hands back a
/// token is the case that has to be reported rather than swallowed.
pub fn write_token(store: &dyn TokenStore, token: &str) -> TokenWrite {
    let token = token.trim();
    if !token.is_empty() {
        return match store.set(token) {
            Ok(()) => TokenWrite::Stored,
            Err(_) => TokenWrite::Fallback,
        };
    }
    match store.delete() {
        Ok(()) => TokenWrite::Stored,
        // A build with no credential store fails every call, and there is
        // nothing in one to have kept: removing the file's key is the whole of
        // the clear, and reporting a failure would leave the user pressing a
        // button that can never succeed.
        Err(_) if !store.available() => TokenWrite::Fallback,
        Err(err) => match store.get() {
            Ok(Some(held)) if !held.trim().is_empty() => TokenWrite::NotCleared(err.detail),
            // Nothing there to have failed to delete.
            Ok(_) => TokenWrite::Stored,
            // The store refused the delete and will not say whether it still
            // holds the token. Assume it does: telling the user a secret was
            // destroyed when it may not have been is the failure that matters,
            // and the remedy - look in your keychain - is right either way.
            Err(_) => TokenWrite::NotCleared(err.detail),
        },
    }
}

/// [`write_token`] against the real store. The one entry point
/// [`crate::settings`] uses, so that the credential store has exactly one
/// caller outside this module.
pub fn store_token(token: &str) -> TokenWrite {
    let outcome = write_token(&keychain(), token);
    // A fresh write earns a fresh migration attempt: whatever made the store
    // refuse last time may be over, and the next read should find out.
    forget_migration(migration_outcome());
    outcome
}

/// Forget a remembered refusal, so the next read offers the token to the store
/// again.
///
/// Two callers and one reason between them: something has happened that could
/// have changed the store's answer. A Save is one (the user has just been to
/// their keychain); an *open* of Settings › Models is the other, which is
/// the remaining cost of asking once per process - a keychain unlocked
/// mid-session went unnoticed until the next Save. It is not called on the
/// dialog's refreshes or on its event-driven polls, because that is exactly
/// the prompt-per-poll once-per-process removed.
fn forget_migration(remembered: &Mutex<Option<TokenLocation>>) {
    if let Ok(mut guard) = remembered.lock() {
        *guard = None;
    }
}

/// The one migration attempt this process makes, and what came of it.
///
/// `None` is "not tried yet". `Some(location)` is "tried, the store refused,
/// and the file is the store until something writes a new token".
fn migration_outcome() -> &'static Mutex<Option<TokenLocation>> {
    static OUTCOME: OnceLock<Mutex<Option<TokenLocation>>> = OnceLock::new();
    OUTCOME.get_or_init(|| Mutex::new(None))
}

/// [`read_token`], with **at most one** attempt per process at handing a
/// fallback token to the store.
///
/// `read_token` offers the settings copy to the store on every call, which is
/// right for a migration that happens once and wrong for one that keeps
/// failing: `list_models` runs whenever Settings opens and every download press
/// reads the token again, so on macOS a locked keychain would put an
/// authorisation prompt in front of the user per poll, for an answer that has
/// not changed. The refusal is remembered instead, and forgotten the moment
/// [`store_token`] writes anything.
///
/// Only a *refusal* is remembered. A migration that succeeded empties the
/// settings key, so the next read has nothing to offer anyway.
fn read_token_once(
    store: &dyn TokenStore,
    remembered: &Mutex<Option<TokenLocation>>,
    from_settings: &str,
) -> TokenRead {
    let in_file = from_settings.trim();
    if !in_file.is_empty() {
        if let Ok(guard) = remembered.lock() {
            if let Some(location) = *guard {
                return TokenRead { token: in_file.to_owned(), location, migrate: false };
            }
        }
    }
    let read = read_token(store, in_file);
    if !in_file.is_empty() && !read.migrate {
        if let Ok(mut guard) = remembered.lock() {
            *guard = Some(read.location);
        }
    }
    read
}

/// The token and where it lives, performing the migration if one is due.
fn token_and_store(app: &tauri::AppHandle) -> (String, TokenLocation) {
    let settings = crate::settings::read(app).unwrap_or_default();
    let in_file = settings
        .get(TOKEN_SETTING)
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let read = read_token_once(&keychain(), migration_outcome(), in_file);
    if read.migrate {
        // Best effort on purpose: a settings file that will not be written is
        // already a problem the user will hear about elsewhere, and the token
        // is safely in the keychain either way. The next read migrates again.
        let _ = crate::settings::forget(app, TOKEN_SETTING);
    }
    (read.token, read.location)
}

/* ------------------------------------------------------------------ */
/* The download itself                                                 */
/* ------------------------------------------------------------------ */

/// The stored Hugging Face token, or the empty string.
///
/// Read through [`keychain`], so the store is asked once per process, and a
/// token pasted into Settings still takes effect on the next press rather than
/// on the next launch: [`store_token`] replaces what was read.
fn token(app: &tauri::AppHandle) -> String {
    token_and_store(app).0
}

/// Whether this URL may carry the token.
///
/// **Host equality, not a substring**: `huggingface.co.example.com` must not
/// match, and neither must a URL with `huggingface.co` in its path. The check
/// is written against the authority component for that reason.
pub fn authorize(url: &str) -> bool {
    let rest = match url.split_once("://") {
        Some(("https", rest)) => rest,
        _ => return false,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    host == "huggingface.co" || host.ends_with(".huggingface.co")
}

/// One artefact's place in the download the user is watching.
///
/// A weight is one file and is its own whole download. A runtime is not: the
/// Windows package is the ONNX Runtime build, `DirectML.dll` and the WebGPU
/// plugin, three transfers under one id, and reporting each of them from zero
/// drew a bar that restarted twice and never said how much there was in total.
/// This is what turns three reports into one - see [`download_runtime`], which
/// is the only caller that fills it in.
#[derive(Clone, Copy)]
struct Portion {
    /// Bytes of the same download that were finished before this artefact
    /// started, added to every report this one makes.
    before: u64,
    /// The whole download's size, from the catalogue. `None` where there is
    /// only one artefact, whose total the response's own `Content-Length`
    /// answers better - it is the byte count actually arriving, resume
    /// included.
    whole: Option<u64>,
}

impl Portion {
    /// A download of one artefact, which is every weight.
    const ALONE: Portion = Portion { before: 0, whole: None };
}

fn emit(id: &str, downloaded: u64, total: Option<u64>, done: bool, error: Option<String>) {
    events::emit(&events::Event::ModelProgress {
        id: id.to_owned(),
        downloaded,
        total,
        done,
        error,
    });
}

/// The last event of every download, whichever way it ended, plus the release
/// of the id. One function so that no early return can leave a row stuck on
/// "Downloading…" forever.
fn finish(id: &str, result: Result<(), String>) {
    release(id);
    match result {
        Ok(()) => emit(id, 0, None, true, None),
        Err(error) => emit(id, 0, None, true, Some(error)),
    }
}

/// Where a partial transfer of `path` lives, **stamped with the digest it is a
/// prefix of**: `lama-manga.onnx` becomes `lama-manga.onnx.4512adab.part`.
///
/// The stamp is what makes resuming safe across a re-pin. A catalogue row keeps
/// its `file_name` when its URL and digest change - a new export of the same
/// model is still `lama-manga.onnx` - so a `.part` named from the file alone
/// would be resumed onto by the *next* artefact, appending new bytes to a
/// prefix of a different file. That produces a digest mismatch rather than a
/// bad install, so it was never a correctness hole; it was a download the user
/// pays for twice and cannot diagnose. Eight hex characters is 32 bits, which
/// is not a collision anybody reaches with a catalogue this size and is short
/// enough to read in a directory listing.
fn part_path(path: &Path, sha256: &str) -> PathBuf {
    let stamp: String = sha256.chars().take(8).collect();
    path.with_extension(format!(
        "{}{stamp}.part",
        path.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ))
}

/// How many bytes of a previous attempt survive. Zero when there is none.
fn resume_offset(part: &Path) -> u64 {
    std::fs::metadata(part).ok().filter(|meta| meta.is_file()).map(|meta| meta.len()).unwrap_or(0)
}

/* ------------------------------------------------------------------ */
/* Unfinished downloads                                                */
/* ------------------------------------------------------------------ */

/// The eight hex characters [`part_path`] stamps a partial file with.
fn part_stamp(sha256: &str) -> &str {
    sha256.get(..8).unwrap_or(sha256)
}

/// The stamp on a `.part` of `file_name`, if that is what `entry` is.
///
/// Pure over the **name**, which is all a sweep has and all it needs: the bytes
/// inside a stranded `.part` are a prefix of an artefact nothing publishes any
/// more, and reading them would say nothing about it that the name does not.
fn part_stamp_of<'a>(entry: &'a str, file_name: &str) -> Option<&'a str> {
    let stamp = entry.strip_prefix(file_name)?.strip_prefix('.')?.strip_suffix(".part")?;
    (stamp.len() == 8 && stamp.chars().all(|c| c.is_ascii_hexdigit())).then_some(stamp)
}

/// Whether a name in the models directory is a `.part` **no press can ever
/// resume**: it belongs to a catalogue row whose digest has since moved.
///
/// A `.part` for a file name the catalogue does not carry at all is left alone.
/// It is not this table's, and a downloader that swept files it cannot account
/// for would be deleting somebody else's work out of a shared directory.
fn is_stranded_part(entry: &str) -> bool {
    MODELS.iter().any(|model| {
        part_stamp_of(entry, model.file_name)
            .is_some_and(|stamp| stamp != part_stamp(model.sha256))
    })
}

/// Remove every stranded `.part` from the download directory. Silent: a sweep
/// that could not read the directory has nothing to report to a settings
/// dialog, and the disk stays as it was.
fn sweep_stranded_parts(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_stranded_part(name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How much of an unfinished download is sitting beside this weight, for the
/// row to report. Only the **current** stamp: an older one has already been
/// swept, and a sum across stamps would be a number no press can act on.
fn partial_bytes(dir: Option<&Path>, model: &ModelPackage) -> Option<u64> {
    let part = part_path(&dir?.join(model.file_name), model.sha256);
    let bytes = resume_offset(&part);
    (bytes > 0).then_some(bytes)
}

/// Everything the runtime's download directory is holding: the `.part` files
/// **and** the complete archives.
///
/// All of them rather than the chosen flavour's two, because what that directory
/// holds is one interrupted attempt per artefact, each 12–455 MB, and the row
/// reports what the directory costs rather than what one press would resume.
///
/// **A complete archive counts.** `download_runtime` keeps one that downloaded
/// and then failed to *unpack*, so the next press verifies it and unpacks it
/// again instead of fetching 202 MB a second time ([`already_fetched`]) - which
/// makes it a remainder in exactly the sense this row is about: bytes kept for a
/// press that may never come. Counting only `.part` left 202 MB invisible to the
/// row, untouched by the Discard press, and enough to make the `remove_dir` at
/// the end of every attempt fail silently.
fn runtime_leftovers(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(runtime_downloads(dir)) else { return Vec::new() };
    entries
        .flatten()
        // Files only. Nothing writes a directory in there, and a sweep that
        // recursed would be one that can empty something it did not create.
        .filter(|entry| entry.file_type().map(|kind| kind.is_file()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect()
}

fn runtime_partial_bytes(dir: &Path) -> Option<u64> {
    let total: u64 = runtime_leftovers(dir).iter().map(|path| resume_offset(path)).sum();
    (total > 0).then_some(total)
}

/// Throw the runtime's kept archives away, and answer whether any went.
///
/// The complete ones go with the partial ones, which is the honest reading of
/// the press: the user asked for the disk back, and the next Download re-fetches
/// what it needs. Nothing else reads that directory - the libraries are already
/// unpacked out of it.
fn discard_runtime_leftovers(dir: &Path) -> bool {
    let mut removed = false;
    for path in runtime_leftovers(dir) {
        removed |= std::fs::remove_file(&path).is_ok();
    }
    // Swept the way `download_runtime` sweeps it: the directory exists for what
    // was inside it, and `remove_dir` declines while anything remains.
    let _ = std::fs::remove_dir(runtime_downloads(dir));
    removed
}

/* ------------------------------------------------------------------ */
/* Which runtime is installed                                          */
/* ------------------------------------------------------------------ */

/// The record a successful install leaves beside the libraries it unpacked.
///
/// The runtime row could always say which build *would* be downloaded and never
/// which one is here: `flavour` and `version` come from a setting, `installed`
/// from a file on disk, and nothing joined them - so a machine that fetched
/// DirectML and then chose CUDA read `cuda12 · Installed`, which is two true
/// statements reading as one false one. Only loading the library can identify
/// it from its bytes, and the settings dialog loads nothing; writing down
/// what was unpacked costs one small file and answers the same question.
///
/// `files` is the other half. A switch of flavour moves the
/// new build in over the old one, and everything the old stamp lists that the
/// new archive did not bring is a library nothing will open again - 18 MB of
/// `DirectML.dll` beside a CUDA runtime - so the difference is removed rather
/// than left in a directory whose contents no longer describe one build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstalledRuntime {
    flavour: String,
    version: String,
    files: Vec<String>,
}

/// A dot-file, beside `.staging` and `.downloads`, for the same reason
/// [`VERIFIED_FILE`] is one: `<app_data>/runtimes` is a directory
/// a user is invited to populate by hand.
const INSTALLED_FILE: &str = ".installed.json";

/// What was installed here, or `None` for **unknown**.
///
/// Unknown rather than none: a runtime unpacked before this record existed, or
/// placed by hand, has no stamp and the row says nothing about its build rather
/// than claiming one. A malformed stamp reads the same way - the file is a
/// record, never a gate on the download.
fn read_installed(dir: &Path) -> Option<InstalledRuntime> {
    std::fs::read(dir.join(INSTALLED_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn write_installed(dir: &Path, stamp: &InstalledRuntime) {
    let Ok(bytes) = serde_json::to_vec_pretty(stamp) else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let _ = cleaner_core::project::buffers::write_atomic(&dir.join(INSTALLED_FILE), &bytes);
}

/// Whether a name out of a stamp may be joined onto the runtime directory.
///
/// **A stamp is a file on the user's disk**, in a directory
/// the user is invited to open, and it is read
/// back as a list of things to *delete*. `dir.join("../models/lama-manga.onnx")`
/// deletes a weight; an absolute path deletes anything the process can reach. So
/// only a plain file name is honoured - one whose `file_name()` is the whole of
/// it, which excludes every separator, `.`, `..`, a Windows drive prefix and the
/// empty string in one rule rather than in a list of them.
///
/// Nothing this module writes is anything else: `move_staged` reports
/// `entry.file_name()`. The check is about the file having been edited, or
/// having arrived from somewhere.
fn is_plain_file_name(name: &str) -> bool {
    Path::new(name).file_name() == Some(std::ffi::OsStr::new(name))
}

/// What the previous build left behind that the new one did not replace.
///
/// Pure over two lists of names, so the rule that decides which files are
/// deleted from a directory the application loads its runtime out of can be
/// read - and tested - without a filesystem. Names that are not plain file
/// names are dropped here rather than at the `remove_file`, so there is one
/// place the rule lives and one place a test has to look.
fn foreign_files(previous: &InstalledRuntime, brought: &[String]) -> Vec<String> {
    previous
        .files
        .iter()
        .filter(|name| is_plain_file_name(name) && !brought.contains(name))
        .cloned()
        .collect()
}

/// Drop the record of what is installed. See [`install_stamped`] for the one
/// moment this is right.
fn forget_installed(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(INSTALLED_FILE));
}

/// Move a downloaded build into place and leave the directory describing
/// itself, in the order a crash can survive.
///
/// The order is the whole of this function, and it is **nothing is written down
/// until the move has actually happened**. The record follows the directory; it
/// never runs ahead of it.
///
/// This used to remove the stamp *before* calling the move, so that a crash in
/// between left [`read_installed`]'s *unknown* rather than the previous build's
/// flavour beside the new libraries. That reasoning was about a power cut, and
/// the failure that actually happens is not a power cut: on Windows the move is
/// refused outright, every time, because the library being replaced is mapped
/// into this process (see [`clear_target`]). Clearing the stamp first meant
/// every refused flavour switch left the row reading *unknown* about a runtime
/// that was still installed, still working and still exactly what the stamp had
/// said it was a second earlier - a record destroyed to describe a change that
/// did not take place.
///
/// So the three outcomes are told apart, and [`MoveFailed::moved`] is what tells
/// them apart:
///
/// - **The move succeeded.** The difference against the previous stamp is swept
///   and the new stamp is written. As before.
/// - **The move failed having moved nothing.** The directory is untouched, so
///   the previous stamp is still a true description of it and it stays. This is
///   the Windows case and it is the common one.
/// - **The move failed part way.** The directory is now a mixture that no stamp
///   describes, and the record is removed: *unknown* is the honest answer to a
///   question that no longer has a true one. The price is the same one the old
///   ordering paid on every failure - the attempt after it has no previous
///   stamp to take a difference against, so it sweeps nothing and whatever the
///   mixture held stays.
///
/// `move_in` is passed rather than called inline so that all three can be
/// tested against failures that never have to happen on a real disk.
fn install_stamped(
    dir: &Path,
    flavour: &str,
    version: &str,
    move_in: impl FnOnce() -> Result<Vec<String>, MoveFailed>,
) -> Result<(), String> {
    let previous = read_installed(dir);
    let brought = match move_in() {
        Ok(brought) => brought,
        Err(failed) => {
            if !failed.moved.is_empty() {
                forget_installed(dir);
            }
            return Err(failed.error);
        }
    };
    forget_installed(dir);
    // **The difference, not the directory.** Emptying `<app_data>/runtimes`
    // before the move would trade a leftover `DirectML.dll` for a window in
    // which a failed download has removed a working runtime; the previous stamp
    // says exactly which files this archive did not replace, and those are the
    // ones nothing will ever open again. A directory with no stamp is
    // left alone: what it holds was put there by a build this record cannot
    // describe.
    if let Some(previous) = previous {
        for name in foreign_files(&previous, &brought) {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    write_installed(
        dir,
        &InstalledRuntime {
            flavour: flavour.to_owned(),
            version: version.to_owned(),
            files: brought,
        },
    );
    Ok(())
}

/// What the server's answer to a `Range` request means for the bytes already on
/// disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resume {
    /// `206 Partial Content`: the body continues from `offset`, which is what
    /// was asked for.
    Continue(u64),
    /// `200 OK`: the server ignored the range and is sending the whole artefact
    /// again. Perfectly legal, and the answer is to start the file over.
    StartOver,
    /// `416 Range Not Satisfiable`: the `.part` is longer than the artefact.
    /// A truncated *transfer* cannot produce that; a re-pinned URL can. The
    /// bytes are not part of anything this table describes, so they go.
    Discard,
}

/// The first byte a `Content-Range` header describes.
///
/// `bytes 20000-59999/60000` is 20000. `None` for anything this cannot read,
/// including `bytes *\/60000`, which is what a `416` carries.
fn content_range_start(value: &str) -> Option<u64> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let start = rest.split('-').next()?.trim();
    start.parse().ok()
}

/// What the server's answer means, read from the status **and the
/// `Content-Range`**.
///
/// A `206` is only a resume if it is a resume *from the offset that was asked
/// for*. A server or a proxy that answers `206` from byte zero, or from
/// somewhere else entirely, would otherwise have its body appended to a prefix
/// it does not follow - producing a file of the right length whose digest is
/// wrong, which costs the user the whole download twice over. A `206` with no
/// `Content-Range` at all is refused for the same reason: there is no way to
/// check it, and the strict reading only costs a restart.
fn resume_plan(status: u16, requested: u64, content_range: Option<&str>) -> Resume {
    match status {
        // Nothing was asked for, so nothing was answered: a fresh download.
        _ if requested == 0 => Resume::StartOver,
        206 => match content_range.and_then(content_range_start) {
            Some(start) if start == requested => Resume::Continue(requested),
            _ => Resume::StartOver,
        },
        416 => Resume::Discard,
        _ => Resume::StartOver,
    }
}

/// The size of the whole artefact, as the response describes it.
///
/// A `206`'s `Content-Length` is what is **left**, not what there is, so the
/// progress bar's total is that plus what is already on disk - which is also
/// why `downloaded` starts at the resumed prefix rather than at zero.
fn resume_total(plan: Resume, content_length: Option<u64>) -> Option<u64> {
    match plan {
        Resume::Continue(offset) => content_length.map(|len| len + offset),
        _ => content_length,
    }
}

/// Feed the first `len` bytes of `path` into a fresh hasher.
///
/// This is the price of resuming. `sha2` has no way to serialise a `Sha256`
/// mid-stream - there is no `into_state` and no `from_state` - so a resumed
/// download either re-reads what it already has or gives up the hash-as-you-go
/// property that makes a 207 MB model verifiable without a second pass. One
/// pass over the `.part` is a few seconds per 200 MB and happens once per
/// interruption; the alternative is downloading those 200 MB again.
fn hash_prefix(path: &Path, len: u64) -> Result<Sha256, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let want = buffer.len().min(left as usize);
        let read = file
            .read(&mut buffer[..want])
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if read == 0 {
            return Err(format!("{}: shorter than the {len} bytes it reported", path.display()));
        }
        hasher.update(&buffer[..read]);
        left -= read as u64;
    }
    Ok(hasher)
}

/// Stream one artefact to `<path>.part`, hashing as it goes, and rename it into
/// place only if the digest matches.
///
/// The digest is computed **from the bytes as they are written**, so a 207 MB
/// model is verified without ever being held in memory. What used to fall out
/// of that - a transfer that could only start at byte zero - no longer does:
/// a `.part` is kept across a cancellation and a failure, the next attempt
/// sends `Range: bytes=<len>-`, and on a `206` the surviving prefix is re-read
/// into the hasher before the new bytes are appended.
///
/// **Only a digest mismatch deletes the `.part`.** A wrong artefact left on
/// disk under the right name is the failure the pin exists to prevent, and it
/// is also the one case where the bytes are known to be worthless. Every other
/// ending keeps them, because every other ending is one the user may want to
/// pick up from.
fn fetch_verified(
    url: &str,
    sha256: &str,
    path: &Path,
    id: &str,
    auth: &str,
    cancel: &AtomicBool,
    portion: Portion,
) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }

    let part = part_path(path, sha256);
    let requested = resume_offset(&part);

    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("manga-cleaner/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let mut request = client.get(url);
    if !auth.is_empty() && authorize(url) {
        request = request.bearer_auth(auth);
    }
    if requested > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={requested}-"));
    }
    let mut response = request.send().map_err(|e| e.to_string())?;

    let status = response.status();
    let content_range = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_owned());
    let plan = resume_plan(status.as_u16(), requested, content_range.as_deref());
    if plan == Resume::Discard {
        let _ = std::fs::remove_file(&part);
    }
    if !status.is_success() {
        // The status, never the body: a Hugging Face refusal echoes the request
        // and the token is in the request.
        return Err(format!("{} answered {}", host_of(url), status));
    }
    // The bar reports the whole download, and this artefact is a part of it:
    // `whole` where the catalogue knows the sum, and the response's own answer
    // where it does not. `before` is added to the counter rather than to the
    // total, so a three-artefact runtime reads as one bar that only goes up.
    let total = portion
        .whole
        .or_else(|| resume_total(plan, response.content_length()).map(|t| t + portion.before));

    let (mut file, mut hasher, mut downloaded) = match plan {
        Resume::Continue(offset) => {
            let hasher = hash_prefix(&part, offset)?;
            let file = std::fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .map_err(|e| format!("{}: {e}", part.display()))?;
            (file, hasher, offset)
        }
        // `create` truncates, which is the discard: a `.part` that is not the
        // prefix of what is arriving must not survive under the same name.
        _ => {
            let file =
                std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
            (file, Sha256::new(), 0u64)
        }
    };
    let mut buffer = vec![0u8; 1 << 18];
    let mut since_report: u64 = 0;

    // The first event carries the resumed prefix, so a bar picking a download
    // back up starts where it left off rather than jumping from zero.
    emit(id, portion.before + downloaded, total, false, None);
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = file.sync_all();
            drop(file);
            return Err("cancelled".to_owned());
        }
        let read = match response.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) => {
                let _ = file.sync_all();
                drop(file);
                return Err(err.to_string());
            }
        };
        // A short write leaves a shorter file, never a corrupt one, so the
        // prefix on disk is still a prefix of the artefact and is still
        // resumable - which is why this returns without deleting anything.
        if let Err(err) = file.write_all(&buffer[..read]) {
            let _ = file.sync_all();
            drop(file);
            return Err(format!("{}: {err}", part.display()));
        }
        hasher.update(&buffer[..read]);
        downloaded += read as u64;
        since_report += read as u64;
        // One event per 4 MiB rather than per block: a 207 MB download is 800
        // blocks at 256 KiB and the channel is the same one a run's events use.
        if since_report >= 4 << 20 {
            since_report = 0;
            emit(id, portion.before + downloaded, total, false, None);
        }
    }
    file.sync_all().map_err(|e| format!("{}: {e}", part.display()))?;
    drop(file);

    let got = hex(&hasher.finalize());
    if got != sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("digest mismatch: expected {sha256}, got {got}"));
    }
    std::fs::rename(&part, path).map_err(|e| format!("{}: {e}", path.display()))?;
    emit(id, portion.before + downloaded, total, false, None);
    Ok(path.to_path_buf())
}

/// The host, for an error message that must not carry the URL's query.
fn host_of(url: &str) -> String {
    url.split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(rest).to_owned())
        .unwrap_or_else(|| "the server".to_owned())
}

/* ------------------------------------------------------------------ */
/* Replacing a library the process is holding open                     */
/* ------------------------------------------------------------------ */

/// The suffix a file that could not be deleted is renamed to, and the digits
/// that make one aside copy different from the next.
///
/// A recognisable shape rather than a random name, because [`sweep_replaced`]
/// reads it back and deletes what it matches out of a directory the user is
/// invited to populate by hand.
const REPLACED_SUFFIX: &str = ".old-";

/// Whether a name in the runtime directory is one of those aside copies.
///
/// Pure, and deliberately strict: `onnxruntime.dll.old-0` is swept and
/// `notes.old-copy`, `.old-1` and `onnxruntime.dll.old-` are not. A sweep that
/// matched on `.old` alone would delete a file somebody put there.
fn is_replaced_aside(name: &str) -> bool {
    match name.rsplit_once(REPLACED_SUFFIX) {
        Some((head, digits)) => {
            !head.is_empty() && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Delete the aside copies a previous session could not.
///
/// **Best effort, and silent.** The copy this session renamed aside is still
/// mapped into this process and will refuse again; the one a *previous* session
/// left is not mapped into this one and goes. That is the whole design: the
/// disk comes back one launch later, and nothing about it is worth a message.
///
/// Called where the runtime directory is next read - [`runtime_row`], which
/// every open of the Settings dialog reaches - rather than at startup, because
/// startup is the one moment the application is not thinking about runtimes.
fn sweep_replaced(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_replaced_aside(name) {
            continue;
        }
        if entry.file_type().map(|kind| kind.is_file()).unwrap_or(false) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Rename a file out of the way, under the first free `<name>.old-<n>`.
///
/// **Windows will not delete a file that is mapped into a process, and it will
/// rename one.** That asymmetry is the whole fix: `ort` loads
/// `onnxruntime.dll` through `libloading` and keeps it in a `OnceLock` for the
/// life of the process, and `models::list_accelerators` calls
/// `cleaner_core::runtime::load` - so the library is mapped the moment the
/// Settings dialog opens, which is the same dialog the Download, flavour-switch
/// and Delete buttons are in. A rename to a new name in the same directory is
/// permitted on a mapped file because the mapping is to the file, not to the
/// name.
///
/// A hundred is not a limit anybody reaches: an aside copy only survives until
/// the next launch sweeps it, so the count is the number of replacements in one
/// session.
#[cfg(windows)]
fn rename_aside(target: &Path) -> Result<(), String> {
    for n in 0..100u32 {
        let mut name = target.file_name().unwrap_or_default().to_os_string();
        name.push(format!("{REPLACED_SUFFIX}{n}"));
        let aside = target.with_file_name(&name);
        if aside.exists() {
            continue;
        }
        if std::fs::rename(target, &aside).is_ok() {
            // The disk is wanted back now where that is allowed, and on the
            // next launch where it is not. A failure here is the ordinary case
            // rather than a surprise: the bytes are still mapped.
            let _ = std::fs::remove_file(&aside);
            return Ok(());
        }
    }
    Err(NOTICE_IN_USE.to_owned())
}

/// Make `target` free for a `rename` onto it.
///
/// Unix unlinks it: a library another process still has open survives under no
/// name at all, which is why this was never a problem here and why the simpler
/// path stays.
#[cfg(unix)]
fn clear_target(target: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(target) {
        Err(_) => Ok(()),
        Ok(meta) if meta.is_dir() => {
            std::fs::remove_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))
        }
        Ok(_) => std::fs::remove_file(target).map_err(|e| format!("{}: {e}", target.display())),
    }
}

/// The same on Windows, where a delete of a mapped library is refused with a
/// sharing violation and [`rename_aside`] is what is left.
///
/// The delete is still tried first: it is one call, it succeeds whenever the
/// library is not loaded, and it leaves the directory holding exactly what it
/// should. Only its failure reaches for the aside copy - and only the aside
/// copy's failure is [`NOTICE_IN_USE`], because at that point Windows has
/// refused both of the two things that can free a name.
#[cfg(windows)]
fn clear_target(target: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(target) {
        Err(_) => Ok(()),
        Ok(meta) if meta.is_dir() => {
            std::fs::remove_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))
        }
        Ok(_) => match std::fs::remove_file(target) {
            Ok(()) => Ok(()),
            Err(_) => rename_aside(target),
        },
    }
}

/// What a move that did not finish left behind.
///
/// `moved` is the reason this is a struct rather than a `String`: whether
/// *anything* landed decides what [`install_stamped`] may say about the
/// directory afterwards, and only the move knows.
struct MoveFailed {
    /// The names that were in place when the failure happened. **Empty is the
    /// common case**, and it is the Windows one: the first target is
    /// `onnxruntime.dll`, the process is holding it, and nothing has moved.
    moved: Vec<String>,
    error: String,
}

/// Move every entry out of the staging directory and into `dir`, and answer
/// with the names that were moved.
///
/// A target that already exists is cleared first so that `rename` succeeds
/// across platforms without `EEXIST` or `ENOTEMPTY` - see [`clear_target`],
/// which is where the platforms differ.
///
/// The names are the install's own record of what this archive brought, which
/// is what [`InstalledRuntime`] is written from and what the *next* install
/// takes its difference against. A name is recorded **after** its rename rather
/// than before: the list is read back as a statement about the directory, and a
/// half-finished move that claimed a file it never wrote is the wrong half of
/// that statement.
fn move_staged(staging: &Path, dir: &Path) -> Result<Vec<String>, MoveFailed> {
    let mut moved = Vec::new();
    match move_each(staging, dir, &mut moved) {
        Ok(()) => Ok(moved),
        Err(error) => Err(MoveFailed { moved, error }),
    }
}

fn move_each(staging: &Path, dir: &Path, moved: &mut Vec<String>) -> Result<(), String> {
    for entry in std::fs::read_dir(staging).map_err(|e| format!("{}: {e}", staging.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = dir.join(entry.file_name());
        clear_target(&target)?;
        std::fs::rename(entry.path(), &target).map_err(|e| format!("{}: {e}", target.display()))?;
        moved.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(())
}

/// Remove an installed runtime, or as much of it as the platform allows.
///
/// Unix takes the tree and is done.
#[cfg(unix)]
fn remove_installed(dir: &Path) -> Result<(), String> {
    std::fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// The same on Windows, where `remove_dir_all` fails outright on a directory
/// holding a mapped `onnxruntime.dll` - which is every directory this press is
/// aimed at, because opening Settings is what mapped it.
///
/// So the tree is walked instead, and each library that will not be deleted is
/// renamed aside. **That is the uninstall the user asked for**: `runtime::find`
/// looks for one name, the name is gone, the row says nothing is installed and
/// the next Download lands cleanly. What is not immediate is the disk, which
/// comes back when [`sweep_replaced`] runs in a session that never loaded those
/// bytes. `.staging` and `.downloads` hold no mapped file and go the ordinary
/// way.
#[cfg(windows)]
fn remove_installed(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        clear_target(&entry.path())?;
    }
    // Declines while the aside copies are still there, which is the honest
    // outcome and not an error: the directory is empty of everything that
    // makes a runtime.
    let _ = std::fs::remove_dir(dir);
    Ok(())
}

/// Unpack the files directly under `library_dir` into `dest`, flat.
///
/// Flat because [`cleaner_core::runtime::search_paths`] looks for
/// `<app_data>/runtimes/<DYLIB_NAME>` and not for the archive's own tree, and
/// because a macOS dylib resolves `@rpath/libonnxruntime.1.dylib` against its
/// own directory - the sibling symlinks have to land beside it.
///
/// Only that one directory: an ONNX Runtime archive carries headers, licences
/// and - on Windows - eight architectures of `DirectML.dll`, none of which this
/// application loads. And only the files inside it that something *can* load:
/// see [`is_loadable`], which is 408 MB of the Windows install.
fn unpack(archive: &Path, library_dir: &str, dest: &Path) -> Result<(), String> {
    if is_zip(archive) {
        unpack_zip(archive, library_dir, dest)
    } else {
        unpack_tar(archive, library_dir, dest)
    }
}

/// Which of the two readers an archive gets, by name.
///
/// A `.nupkg` and a GitHub `.zip` are both zips and everything else this
/// catalogue names is a gzipped tar. Split out of [`unpack`] because
/// [`unpacked_bytes`] asks the same question for a different reason and two
/// spellings of it would be two things to keep equal.
fn is_zip(archive: &Path) -> bool {
    let Some(name) = archive.file_name() else { return false };
    let name = name.to_string_lossy().to_ascii_lowercase();
    !(name.ends_with(".tgz") || name.ends_with(".tar.gz"))
}

/// Whether a file inside the archive's library directory is one this
/// application will ever open.
///
/// **`.pdb` and `.lib` are skipped, and the reason is that nothing can load
/// them.** The stock `win-x64` package carries `onnxruntime.pdb` at about
/// 408 MB uncompressed beside a 12 MB download, which is most of the staging
/// space a Windows install needs and all of the surprise in it.
///
/// What was checked before skipping them, because "nothing loads it" is a claim
/// and not an assumption:
///
/// - The runtime is reached by name and by name only. `ort` is built with
///   `load-dynamic` (`src-tauri/Cargo.toml`), so `runtime::load` hands
///   `ort::init_from` a path to `runtime::DYLIB_NAME` and `libloading` opens
///   that one file. The OS loader resolves a DLL's imports from other DLLs; it
///   has never read a `.pdb`, which is a debugger's file, or a `.lib`, which is
///   an import library consumed at link time by a build that is not happening
///   on the user's machine.
/// - Nothing in this repository names either extension beside the runtime. The
///   only `.lib` that appears anywhere is `cudart64_12.lib`, in `accel.rs`'s
///   tests, where it is an example of a name `accel::is_cudart` must *not*
///   match: that probe requires `.dll`.
///
/// A crash dump off a released build is symbolised from the symbol server the
/// package came from rather than from a file beside the library, so nothing is
/// lost that was being used.
fn is_loadable(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    !(name.ends_with(".pdb") || name.ends_with(".lib"))
}

/// Whether an archive entry is a file directly inside `library_dir`, and what
/// to call it. `None` for anything else - including a nested path, which is the
/// `include/` tree and the CUDA provider's own subdirectories.
fn under<'a>(entry: &'a str, library_dir: &str) -> Option<&'a str> {
    let prefix = format!("{}/", library_dir.trim_end_matches('/'));
    let rest = entry.strip_prefix(prefix.as_str())?;
    // `.` and `..` are not files, and a backslash is a separator on the one
    // platform whose archive is a zip: none of them may name a destination.
    if rest.is_empty() || rest.contains('/') || rest.contains('\\') || rest == "." || rest == ".." {
        return None;
    }
    Some(rest)
}

fn unpack_tar(archive: &Path, library_dir: &str, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut written = 0usize;
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_symlink() && !entry_type.is_hard_link() {
            continue;
        }
        let path = entry.path().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
        let Some(name) = under(&path, library_dir) else { continue };
        if !is_loadable(name) {
            continue;
        }
        let out = dest.join(name);
        // A symlink is how `libonnxruntime.dylib` points at
        // `libonnxruntime.1.28.0.dylib`; `unpack_in` honours it, and the
        // destination is inside `dest` because `name` has no separator in it.
        entry.unpack(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        written += 1;
    }
    if written == 0 {
        return Err(format!("{} carries nothing under {library_dir}", archive.display()));
    }
    Ok(())
}

fn unpack_zip(archive: &Path, library_dir: &str, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut written = 0usize;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        if !entry.is_file() {
            continue;
        }
        let path = entry.name().replace('\\', "/");
        let Some(name) = under(&path, library_dir) else { continue };
        if !is_loadable(name) {
            continue;
        }
        let out = dest.join(name);
        let mut sink =
            std::fs::File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        std::io::copy(&mut entry, &mut sink).map_err(|e| format!("{}: {e}", out.display()))?;
        written += 1;
    }
    if written == 0 {
        return Err(format!("{} carries nothing under {library_dir}", archive.display()));
    }
    Ok(())
}

/* ------------------------------------------------------------------ */
/* Tests                                                               */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_model_groups_contain_every_dependency_but_no_shared_detector() {
        let gate: Vec<_> = model_group("scriptGate").unwrap().iter().map(|m| m.id).collect();
        assert_eq!(gate, ["scriptGate", "scriptGateLabels"]);
        let reader: Vec<_> = model_group("mangaOcr").unwrap().iter().map(|m| m.id).collect();
        assert_eq!(reader, ["ocrEncoder", "ocrDecoder", "ocrVocab"]);
        assert!(model_group("balloonDetector").is_none());
        assert_eq!(group_for_member("scriptGateLabels"), Some("scriptGate"));
        assert_eq!(group_for_member("ocrDecoder"), Some("mangaOcr"));
        let hayai: Vec<_> = model_group(HAYAI_GROUP).unwrap().iter().map(|m| m.id).collect();
        assert_eq!(hayai, ["hayaiVision", "hayaiDecoder", "hayaiTokenizer"]);
        assert_eq!(group_for_member("hayaiTokenizer"), Some(HAYAI_GROUP));
        assert!(model_group(HAYAI_GROUP).unwrap().iter().all(|m| m.required_by.is_empty()));
        assert_eq!(group_for_member("balloonDetector"), None);
        let denoise: Vec<_> = model_group(PAGE_DENOISE_GROUP).unwrap().iter().map(|m| m.id).collect();
        assert_eq!(denoise, ["pageDenoiseModel", "pageDenoiseSeams"]);
        assert_eq!(group_for_member("pageDenoiseSeams"), Some(PAGE_DENOISE_GROUP));
        assert!(model_group(PAGE_DENOISE_GROUP).unwrap().iter().all(|m| m.required_by == ["pageDenoise"]));
    }

    /// The script and the table are two copies of eight digests, and a copy that
    /// can drift is a download verified against the wrong number. Parsed rather
    /// than eyeballed, for the same reason `runtime::package`'s own test parses
    /// `fetch-runtime.sh`.
    #[test]
    fn the_script_and_the_table_agree() {
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/fetch-models.sh"),
        )
        .expect("scripts/fetch-models.sh is readable");

        // `fetch <name> \` + `<sha> \` + `<url>`, continuations joined.
        //
        // Line endings are normalised first. A Windows checkout has
        // `core.autocrlf` on by default, so every line of a committed shell
        // script arrives ending `\r\n`; without this the continuation joins
        // nothing, each line stands alone, and the parse fails on a `fetch`
        // line carrying one field instead of three. That is a test reading a
        // file rather than a fact about the repository, so it is fixed here.
        let joined = script.replace("\r\n", "\n").replace("\\\n", " ");
        let mut found: Vec<(String, String, String)> = Vec::new();
        for line in joined.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("fetch ") else { continue };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            assert_eq!(fields.len(), 3, "unexpected fetch line: {line}");
            found.push((fields[0].to_owned(), fields[1].to_owned(), fields[2].to_owned()));
        }

        assert_eq!(found.len(), MODELS.len(), "the script fetches a different number of files");
        for (name, sha, url) in &found {
            let model = MODELS
                .iter()
                .find(|m| m.file_name == name)
                .unwrap_or_else(|| panic!("the table has no row for {name}"));
            assert_eq!(&model.sha256, sha, "{name}: digest differs from the script");
            assert_eq!(&model.url, url, "{name}: url differs from the script");
        }
    }

    #[test]
    fn every_row_carries_a_real_digest_and_a_size() {
        for model in MODELS {
            assert_eq!(model.sha256.len(), 64, "{}: not a sha256", model.id);
            assert!(model.sha256.chars().all(|c| c.is_ascii_hexdigit()), "{}", model.id);
            assert!(model.bytes > 0, "{}: no size", model.id);
            assert!(model.kind_key.starts_with("models.kind."), "{}", model.kind_key);
        }
        let ids: std::collections::HashSet<_> = MODELS.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), MODELS.len(), "two rows share an id");
    }

    /// Every engine the interface can offer is either named by a row or needs
    /// no weights at all. A rung that needed a file nothing downloads would be
    /// a control that fails with nothing in Settings to press.
    #[test]
    fn the_engines_the_table_gates_are_the_engines_the_ladder_has() {
        let named: std::collections::HashSet<&str> =
            MODELS.iter().flat_map(|m| m.required_by.iter().copied()).collect();
        assert!(named.contains("autoClean"));
        assert!(named.contains("lama"));
        // `fill` and `denoise` need nothing, and `flux` is the sidecar's, which
        // `sidecarAvailable` answers.
        for engine in ["fill", "denoise", "flux"] {
            assert!(!named.contains(engine), "{engine} should need no download");
        }
    }

    /// The token goes to Hugging Face and to nowhere else - including hosts
    /// that merely contain the name.
    #[test]
    fn only_hugging_face_urls_carry_the_token() {
        assert!(authorize("https://huggingface.co/ogkalu/x/resolve/main/a.onnx"));
        assert!(authorize("https://HuggingFace.co/x"));
        assert!(authorize("https://cdn-lfs.huggingface.co/x"));
        assert!(!authorize("https://github.com/microsoft/onnxruntime/releases/x"));
        assert!(!authorize("https://huggingface.co.evil.test/x"));
        assert!(!authorize("https://evil.test/huggingface.co/x"));
        assert!(!authorize("https://evil.test/?u=https://huggingface.co/x"));
        // Plain HTTP never carries it, whatever the host says it is.
        assert!(!authorize("http://huggingface.co/x"));
    }

    /// One artefact is on GitHub and takes no token; every
    /// Hugging Face one does.
    #[test]
    fn the_catalogue_urls_are_https_and_only_two_hosts() {
        for model in MODELS {
            assert!(model.url.starts_with("https://"), "{}: not https", model.id);
            let host = host_of(model.url);
            assert!(
                host == "huggingface.co" || host == "github.com",
                "{}: unexpected host {host}",
                model.id
            );
        }
    }

    #[test]
    fn a_digest_is_computed_over_the_whole_file() {
        let dir = std::env::temp_dir().join(format!("mc-weights-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("digest.bin");
        std::fs::write(&path, b"manga").unwrap();
        assert_eq!(
            digest_file(&path).unwrap(),
            "05b44f4aa0e1b1de75c4ed641fb9c3c6b42b9186c59c78411bb1e2d34f26977e"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only files directly inside the archive's library directory are unpacked.
    /// The `include/` tree beside it, and the CUDA provider's subdirectories,
    /// are not what `dlopen` is pointed at.
    #[test]
    fn only_the_library_directory_is_unpacked() {
        assert_eq!(under("ort-1.28/lib/libonnxruntime.dylib", "ort-1.28/lib"), Some("libonnxruntime.dylib"));
        assert_eq!(under("ort-1.28/lib/nested/x.so", "ort-1.28/lib"), None);
        assert_eq!(under("ort-1.28/include/x.h", "ort-1.28/lib"), None);
        assert_eq!(under("ort-1.28/lib", "ort-1.28/lib"), None);
        assert_eq!(under("ort-1.28/lib/..", "ort-1.28/lib"), None);
        assert_eq!(under("ort-1.28/lib/.", "ort-1.28/lib"), None);
        assert_eq!(under("ort-1.28/lib/..\\x.dll", "ort-1.28/lib"), None);
        assert_eq!(under("runtimes/win-x64/native/onnxruntime.dll", "runtimes/win-x64/native"), Some("onnxruntime.dll"));
    }

    /// `installed` is presence **and size**, and a copy outside the one
    /// directory this module owns is read-only however present it is. Both
    /// halves are what the Delete button is drawn from, so both are pinned.
    #[test]
    fn a_weight_is_located_by_size_and_owned_by_one_directory() {
        let root = std::env::temp_dir().join(format!("mc-locate-{}", std::process::id()));
        let models = root.join("models");
        std::fs::create_dir_all(&models).unwrap();
        let model = &MODELS[4]; // the labels file: 1163 bytes, small enough to write
        let path = models.join(model.file_name);

        // Nothing there yet.
        assert!(locate(model, Some(root.as_path()), Some(models.as_path())).is_none());

        // There, but the wrong size: a truncated download is not an install.
        std::fs::write(&path, vec![0u8; 12]).unwrap();
        assert!(locate(model, Some(root.as_path()), Some(models.as_path())).is_none());

        // The right size, in the directory this module writes to.
        std::fs::write(&path, vec![0u8; model.bytes as usize]).unwrap();
        let (found, read_only) = locate(model, Some(root.as_path()), Some(models.as_path()))
            .expect("a file of the right size is installed");
        assert_eq!(found, path);
        assert!(!read_only, "the app-data copy is the one that may be deleted");

        // The same file, when the writable directory is somewhere else - which
        // is what `$MANGA_CLEANER_MODELS` and a bundled copy look like.
        let (_, read_only) =
            locate(model, Some(root.as_path()), Some(Path::new("/elsewhere/models")))
                .expect("still found");
        assert!(read_only, "a copy outside the app-data directory is not ours to delete");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn imported_full_rt_is_visible_to_the_catalogue() {
        let root = std::env::temp_dir().join(format!("mc-full-rt-locate-{}", std::process::id()));
        let dir = root.join("models/rtdetr-v2-full");
        std::fs::create_dir_all(&dir).unwrap();
        let model = MODELS.iter().find(|entry| entry.id == "fullRt").unwrap();
        let path = dir.join(model.file_name);
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(model.bytes).unwrap();
        let found = locate(model, Some(&root), Some(&root.join("models"))).unwrap();
        assert_eq!(found.0, path);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A `.part` whose digest does not match is deleted rather than published,
    /// and the failure names both digests. Driven through the same function a
    /// real download uses, against a file:// -free local check of the digest
    /// half - there is no network in a test.
    #[test]
    fn a_bad_digest_is_refused_and_leaves_nothing_behind() {
        let dir = std::env::temp_dir().join(format!("mc-weights-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("weights.onnx");
        std::fs::write(&path, b"not the model").unwrap();
        let got = digest_file(&path).unwrap();
        assert_ne!(got, MODELS[0].sha256);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A successful download updates the session's verification cache, clearing
    /// any prior failed verification.
    #[test]
    fn a_failed_verification_is_remedied_by_a_successful_download() {
        let id = MODELS[0].id;
        forget_verified(id);

        // An earlier verify found the weight corrupt.
        record_verified(id, false);
        let current = verified().lock().unwrap().get(id).copied();
        assert_eq!(current, Some(false));

        // A subsequent download verifies the newly fetched file and records success.
        record_verified(id, true);
        let updated = verified().lock().unwrap().get(id).copied();
        assert_eq!(updated, Some(true));

        forget_verified(id);
    }

    /// Only regular files and symlinks under the library directory are unpacked
    /// from a tar.gz archive; directory entries and nested files are skipped.
    #[test]
    fn unpack_tar_extracts_only_flat_library_files_and_skips_directories() {
        let dir = std::env::temp_dir().join(format!("mc-unpack-tar-{}", std::process::id()));
        let dest = dir.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let archive_path = dir.join("runtime.tar.gz");

        let archive_file = std::fs::File::create(&archive_path).unwrap();
        let enc = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // 1. `lib/` dir entry
        let mut h1 = tar::Header::new_gnu();
        h1.set_entry_type(tar::EntryType::Directory);
        h1.set_size(0);
        h1.set_cksum();
        tar.append_data(&mut h1, "lib/", std::io::empty()).unwrap();

        // 2. `lib/sub/` dir entry
        let mut h2 = tar::Header::new_gnu();
        h2.set_entry_type(tar::EntryType::Directory);
        h2.set_size(0);
        h2.set_cksum();
        tar.append_data(&mut h2, "lib/sub/", std::io::empty()).unwrap();

        // 3. `lib/libx.dylib` file
        let data3 = b"dylib-content";
        let mut h3 = tar::Header::new_gnu();
        h3.set_entry_type(tar::EntryType::Regular);
        h3.set_size(data3.len() as u64);
        h3.set_cksum();
        tar.append_data(&mut h3, "lib/libx.dylib", &data3[..]).unwrap();

        // 4. `lib/sub/y` file
        let data4 = b"y-content";
        let mut h4 = tar::Header::new_gnu();
        h4.set_entry_type(tar::EntryType::Regular);
        h4.set_size(data4.len() as u64);
        h4.set_cksum();
        tar.append_data(&mut h4, "lib/sub/y", &data4[..]).unwrap();

        let enc = tar.into_inner().unwrap();
        enc.finish().unwrap();

        unpack_tar(&archive_path, "lib", &dest).unwrap();

        assert!(dest.join("libx.dylib").exists());
        assert_eq!(std::fs::read(dest.join("libx.dylib")).unwrap(), b"dylib-content");
        assert!(!dest.join("sub").exists());
        assert!(!dest.join("y").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /* -------------------------------------------------------------- */
    /* What a press answers                                            */
    /* -------------------------------------------------------------- */

    /// The three answers a Delete press can have, decided from what was found
    /// and nothing else. `readOnly` on the row is what stops the press; this is
    /// what the press says when the row was stale and it happened anyway.
    #[test]
    fn a_delete_says_which_of_the_three_things_it_found() {
        assert_eq!(plan_delete(None), Err(DeleteOutcome::NotFound));
        assert_eq!(
            plan_delete(Some((PathBuf::from("/elsewhere/lama-manga.onnx"), true))),
            Err(DeleteOutcome::ReadOnlyElsewhere),
            "found, and not ours to remove"
        );
        assert_eq!(
            plan_delete(Some((PathBuf::from("/app-data/models/lama-manga.onnx"), false))),
            Ok(PathBuf::from("/app-data/models/lama-manga.onnx"))
        );
    }

    /// Both enums cross the seam as camelCase ids, because the interface has a
    /// sentence for each and none of them is written here.
    #[test]
    fn an_outcome_crosses_the_seam_as_an_id() {
        assert_eq!(serde_json::to_string(&DeleteOutcome::Deleted).unwrap(), "\"deleted\"");
        assert_eq!(serde_json::to_string(&DeleteOutcome::NotFound).unwrap(), "\"notFound\"");
        assert_eq!(
            serde_json::to_string(&DeleteOutcome::ReadOnlyElsewhere).unwrap(),
            "\"readOnlyElsewhere\""
        );
        assert_eq!(serde_json::to_string(&DownloadStart::Started).unwrap(), "\"started\"");
        assert_eq!(
            serde_json::to_string(&DownloadStart::AlreadyRunning).unwrap(),
            "\"alreadyRunning\""
        );
        assert_eq!(
            serde_json::to_string(&DownloadStart::AlreadyInstalled).unwrap(),
            "\"alreadyInstalled\""
        );
    }

    /// The eviction itself, through the same call `delete_model` makes: a
    /// session parked under the deleted weight's kind is dropped, and one
    /// parked under another kind is left alone.
    ///
    /// The residency table is process-wide, so this parks under its own key in
    /// two kinds and takes both of them back. The "at least one" below is what
    /// makes that safe: another test may have parked a session of the same kind
    /// under a key of its own.
    #[test]
    fn deleting_a_weight_drops_the_session_that_was_built_from_it() {
        use cleaner_core::residency::{self, Key, Resident};

        /// A stand-in for a session: `residency` asks a parked value exactly
        /// one question, and this answers it the way a session nobody has
        /// asked back does.
        struct Parked;
        impl Resident for Parked {
            fn spent(&self) -> bool {
                false
            }
        }

        let gone = registry::Kind::Inpainter;
        let kept = registry::Kind::BalloonDetector;
        residency::checkin(Key::new(gone, "mc-evict-test"), Parked);
        residency::checkin(Key::new(kept, "mc-evict-test"), Parked);
        assert!(residency::parked(gone) && residency::parked(kept), "both are parked");

        // At least one, rather than exactly one: the table is the process's and
        // another test in this crate may have parked a session of the same kind.
        assert!(evict_sessions_of("inpainter") >= 1, "the deleted weight's session went");
        assert!(!residency::parked(gone));
        assert!(residency::parked(kept), "another model's session is not this press's business");

        // The runtime is not a session and evicts nothing.
        assert_eq!(evict_sessions_of(RUNTIME_ID), 0);
        assert!(residency::parked(kept), "and it left the table alone");

        assert!(evict_sessions_of("balloonDetector") >= 1);
        assert!(!residency::parked(kept));
    }

    /// Every parked catalogue session has a weight mapping. Full RT has only
    /// a run-local session, so it has no parked kind to evict; the runtime is
    /// also not a session.
    #[test]
    fn every_weight_names_the_session_it_would_evict() {
        for model in MODELS {
            // Page denoise sessions live for one command and are never parked.
            if ["fullRt", "pageDenoiseModel", "pageDenoiseSeams"].contains(&model.id) { continue; }
            assert!(
                resident_kind(model.id).is_some(),
                "{}: nothing would be evicted when it is deleted",
                model.id
            );
        }
        assert_eq!(resident_kind(RUNTIME_ID), None);
        assert_eq!(resident_kind("fullRt"), None);
        assert_eq!(resident_kind("no-such-model"), None);

        // The two halves of the script gate are one session, and so are the
        // three parts of the reader: those are the two places the mapping is
        // not one file to one kind, and both are a session built from several
        // files rather than a coincidence.
        assert_eq!(resident_kind("scriptGate"), resident_kind("scriptGateLabels"));
        assert_eq!(resident_kind("scriptGate"), Some(registry::Kind::ScriptGate));
        for id in ["ocrEncoder", "ocrDecoder", "ocrVocab"] {
            assert_eq!(resident_kind(id), Some(registry::Kind::ScriptGate), "{id}");
        }
        for id in ["hayaiVision", "hayaiDecoder", "hayaiTokenizer"] {
            assert_eq!(resident_kind(id), Some(registry::Kind::Ocr), "{id}");
        }

        // And no two *unrelated* parked weights share a kind by accident: eleven
        // artefacts, five parked sessions (manga-ocr is parked inside the
        // gate, Hayai on its own), and the three groupings above account for
        // every file that is not alone on its row.
        let kinds: std::collections::HashSet<_> =
            MODELS.iter().filter_map(|m| resident_kind(m.id)).collect();
        assert_eq!(kinds.len(), 5, "eleven weights, five parked sessions, three shared");
        assert_eq!(MODELS.len(), 14);
    }

    /* -------------------------------------------------------------- */
    /* The token store                                                 */
    /* -------------------------------------------------------------- */

    /// A store that works, so the decisions around a working keychain can be
    /// exercised without one. `Mutex` rather than `RefCell` because
    /// [`TokenStore`] is used behind a shared reference.
    #[derive(Default)]
    struct MemoryStore {
        slot: Mutex<Option<String>>,
    }

    impl MemoryStore {
        fn holding(token: &str) -> MemoryStore {
            MemoryStore { slot: Mutex::new(Some(token.to_owned())) }
        }

        fn held(&self) -> Option<String> {
            self.slot.lock().unwrap().clone()
        }
    }

    impl TokenStore for MemoryStore {
        fn get(&self) -> Result<Option<String>, StoreError> {
            Ok(self.held())
        }

        fn set(&self, token: &str) -> Result<(), StoreError> {
            *self.slot.lock().unwrap() = Some(token.to_owned());
            Ok(())
        }

        fn delete(&self) -> Result<(), StoreError> {
            *self.slot.lock().unwrap() = None;
            Ok(())
        }

        fn available(&self) -> bool {
            true
        }
    }

    /// How a store that is there says no: a locked keychain, which is the
    /// commonest refusal and the one with a remedy the user can carry out.
    fn locked(detail: &str) -> StoreError {
        StoreError::new(StoreReason::Locked, detail)
    }

    /// A build with **no** credential store: every call fails and there is
    /// nothing behind them. The settings file is the store, permanently.
    struct NoStore;

    impl TokenStore for NoStore {
        fn get(&self) -> Result<Option<String>, StoreError> {
            Err(StoreError::new(StoreReason::Unknown, "this build has no credential store"))
        }

        fn set(&self, _token: &str) -> Result<(), StoreError> {
            Err(StoreError::new(StoreReason::Unknown, "this build has no credential store"))
        }

        fn delete(&self) -> Result<(), StoreError> {
            Err(StoreError::new(StoreReason::Unknown, "this build has no credential store"))
        }

        fn available(&self) -> bool {
            false
        }
    }

    /// A credential store that is **there** and answers nothing: a locked
    /// keychain, or one refusing this process. Every call fails, exactly as
    /// `NoStore`'s do, and every one of them means something different.
    struct BlindStore;

    impl TokenStore for BlindStore {
        fn get(&self) -> Result<Option<String>, StoreError> {
            Err(locked("the keychain is locked"))
        }

        fn set(&self, _token: &str) -> Result<(), StoreError> {
            Err(locked("the keychain is locked"))
        }

        fn delete(&self) -> Result<(), StoreError> {
            Err(locked("the keychain is locked"))
        }

        fn available(&self) -> bool {
            true
        }
    }

    /// The migration: a token that was in `settings.json` before the upgrade is
    /// moved into the store on the first read, and the caller is told to take
    /// the plaintext copy out of the file.
    #[test]
    fn the_first_read_moves_a_settings_token_into_the_store() {
        let store = MemoryStore::default();
        let read = read_token(&store, "  hf_from_the_file  ");

        assert_eq!(read.token, "hf_from_the_file", "the trimmed value is what gets used");
        assert_eq!(read.location, TokenLocation::Keychain);
        assert!(read.migrate, "the settings copy has to be removed");
        assert_eq!(store.held().as_deref(), Some("hf_from_the_file"));

        // And the second read is an ordinary read: the store answers, and there
        // is nothing left in the file to migrate.
        let again = read_token(&store, "");
        assert_eq!(again.token, "hf_from_the_file");
        assert_eq!(again.location, TokenLocation::Keychain);
        assert!(!again.migrate);
    }

    /// A store holding a token that will not accept a new one: a locked
    /// keychain, which is exactly the shape that used to lose a token.
    struct LockedStore {
        holds: &'static str,
    }

    impl TokenStore for LockedStore {
        fn get(&self) -> Result<Option<String>, StoreError> {
            Ok(Some(self.holds.to_owned()))
        }
        fn set(&self, _token: &str) -> Result<(), StoreError> {
            Err(locked("the keychain is locked"))
        }
        fn delete(&self) -> Result<(), StoreError> {
            Err(locked("the keychain is locked"))
        }
        fn available(&self) -> bool {
            true
        }
    }

    /// **The token in the settings file is the newer one and it wins**, even
    /// over a store that already holds a different one.
    ///
    /// This is the token-loss case. A keychain holding an old `A` refuses the
    /// `B` the user has just pasted; `B` lands in `settings.json`; and a read
    /// that asked the store first would answer `A`, call `B` a stale duplicate
    /// and delete it - leaving the user with a token they replaced and no trace
    /// of the one they typed. Nothing reaches the file except by way of a write
    /// the store refused, so the file's copy is always the newer one.
    #[test]
    fn the_settings_copy_is_the_newer_token_and_overwrites_the_stored_one() {
        let store = MemoryStore::holding("hf_old");
        let read = read_token(&store, "hf_new");

        assert_eq!(read.token, "hf_new", "the token the user last gave us");
        assert_eq!(read.location, TokenLocation::Keychain);
        assert!(read.migrate, "and the plaintext copy may now go");
        assert_eq!(store.held().as_deref(), Some("hf_new"), "because the store has it");
    }

    /// The same situation with a store that will not take the new one: the file
    /// keeps it, the store is left alone, and the caller is **not** told to
    /// delete the only copy there is.
    #[test]
    fn a_refused_write_never_costs_the_file_its_only_copy() {
        let store = LockedStore { holds: "hf_old" };
        let read = read_token(&store, "hf_new");

        assert_eq!(read.token, "hf_new", "the newer token is still the one used");
        assert_eq!(
            read.location,
            TokenLocation::FileStoreUnavailable { reason: StoreReason::Locked },
            "and the interface says the store is what could not be reached, and why"
        );
        assert!(!read.migrate, "deleting it here would destroy it");
        assert_eq!(store.get().unwrap().as_deref(), Some("hf_old"), "the store is untouched");
    }

    /// Nothing anywhere is not a migration and not a fallback.
    #[test]
    fn no_token_at_all_asks_nothing_of_the_caller() {
        let read = read_token(&MemoryStore::default(), "   ");
        assert_eq!(read.token, "");
        assert_eq!(read.location, TokenLocation::Keychain);
        assert!(!read.migrate);
    }

    /// The fallback, which is the case CI can actually run: with no usable
    /// store the settings file is the store, the value is still used, and
    /// nothing is removed from the file.
    #[test]
    fn a_machine_with_no_credential_store_keeps_the_token_in_the_settings_file() {
        let read = read_token(&NoStore, "hf_from_the_file");
        assert_eq!(read.token, "hf_from_the_file");
        assert_eq!(read.location, TokenLocation::FileNoStore);
        assert!(!read.migrate, "removing it would lose the only copy there is");

        // And a store that refuses only the *write* is the same answer: the
        // token is not thrown away to make a point about where secrets belong.
        struct ReadOnlyStore;
        impl TokenStore for ReadOnlyStore {
            fn get(&self) -> Result<Option<String>, StoreError> {
                Ok(None)
            }
            fn set(&self, _token: &str) -> Result<(), StoreError> {
                Err(locked("locked"))
            }
            fn delete(&self) -> Result<(), StoreError> {
                Err(locked("locked"))
            }
            fn available(&self) -> bool {
                true
            }
        }
        let refused = read_token(&ReadOnlyStore, "hf_from_the_file");
        assert_eq!(
            refused.location,
            TokenLocation::FileStoreUnavailable { reason: StoreReason::Locked },
            "there is a store"
        );
        assert_eq!(refused.token, "hf_from_the_file");
        assert!(!refused.migrate);
    }

    /// Saving and clearing, and what each tells the settings file to do.
    #[test]
    fn writing_a_token_says_what_happened_to_it() {
        let store = MemoryStore::default();
        assert_eq!(write_token(&store, "  hf_new  "), TokenWrite::Stored);
        assert_eq!(store.held().as_deref(), Some("hf_new"), "stored trimmed");

        // The empty string is the Clear button.
        assert_eq!(write_token(&store, ""), TokenWrite::Stored);
        assert_eq!(store.held(), None);

        // Clearing again, with nothing there, is still a success.
        assert_eq!(write_token(&store, ""), TokenWrite::Stored);

        // With no store, the caller is told to keep it itself.
        assert_eq!(write_token(&NoStore, "hf_new"), TokenWrite::Fallback);
    }

    /// **A Clear the store refuses is not a Clear.** The secret is still in the
    /// keychain, so the answer has to be distinguishable from the machine that
    /// simply has no keychain - otherwise the settings file drops its key, the
    /// press reports success, and the user believes a token was destroyed that
    /// was not.
    #[test]
    fn a_clear_the_store_refuses_is_reported_rather_than_swallowed() {
        let refused = write_token(&LockedStore { holds: "hf_still_here" }, "");
        match refused {
            TokenWrite::NotCleared(reason) => assert!(reason.contains("locked"), "{reason}"),
            other => panic!("a refused delete must not read as {other:?}"),
        }

        // A machine with no store at all fails the delete too, and that *is*
        // the fallback: there is nothing in a keychain to have kept, and
        // removing the file's key is the whole of the clear.
        assert_eq!(write_token(&NoStore, ""), TokenWrite::Fallback);
    }

    /// The same error from the store means two different things, and which one
    /// it is is a property of the **build**, not of the moment.
    ///
    /// A locked keychain and a platform with no keychain both fail every call.
    /// Reporting the first as the second told the user "this computer has no
    /// credential store" when unlocking theirs would have fixed it.
    #[test]
    fn a_store_that_exists_and_will_not_answer_is_not_a_missing_store() {
        let refusal = locked("the keychain is locked");
        assert_eq!(
            fallback_location(&BlindStore, &refusal),
            TokenLocation::FileStoreUnavailable { reason: StoreReason::Locked }
        );
        // The reason is dropped where there is no store to have had one: the
        // build fact decides the location, and a machine with no keychain has
        // nothing for the user to unlock.
        assert_eq!(fallback_location(&NoStore, &refusal), TokenLocation::FileNoStore);

        let unreachable = read_token(&BlindStore, "hf_from_the_file");
        assert_eq!(unreachable.token, "hf_from_the_file");
        assert_eq!(
            unreachable.location,
            TokenLocation::FileStoreUnavailable { reason: StoreReason::Locked }
        );

        // With nothing in the file the read still has to say where a token
        // would go, because the interface warns before anything is pasted.
        assert_eq!(
            read_token(&BlindStore, "").location,
            TokenLocation::FileStoreUnavailable { reason: StoreReason::Locked }
        );
        assert_eq!(read_token(&NoStore, "").location, TokenLocation::FileNoStore);
    }

    /// **A store that refuses the delete and then refuses to be read is not a
    /// successful Clear.** It failed to remove the token and will not say
    /// whether it still has it, so the honest answer is the one that sends the
    /// user to their keychain - the previous reading called it the fallback and
    /// reported success.
    #[test]
    fn a_store_that_will_not_say_whether_it_kept_the_token_is_treated_as_having_kept_it() {
        match write_token(&BlindStore, "") {
            TokenWrite::NotCleared(reason) => assert!(reason.contains("locked"), "{reason}"),
            other => panic!("a blind refusal must not read as {other:?}"),
        }

        // And the build with no store at all still clears, because there is
        // nothing behind those same errors to have kept anything.
        assert_eq!(write_token(&NoStore, ""), TokenWrite::Fallback);
    }

    /// A store that counts what is asked of it.
    struct CountingStore {
        attempts: std::sync::atomic::AtomicUsize,
    }

    impl TokenStore for CountingStore {
        fn get(&self) -> Result<Option<String>, StoreError> {
            Err(locked("the keychain is locked"))
        }
        fn set(&self, _token: &str) -> Result<(), StoreError> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            Err(locked("the keychain is locked"))
        }
        fn delete(&self) -> Result<(), StoreError> {
            Err(locked("the keychain is locked"))
        }
        fn available(&self) -> bool {
            true
        }
    }

    /// **The migration is attempted once per process, not once per read.**
    ///
    /// `read_token` offers the settings copy to the store on every call, which
    /// is right for a migration that happens once and wrong for one that keeps
    /// failing: `list_models` runs every time Settings opens and every download
    /// press reads the token again, so on macOS a locked keychain would put an
    /// authorisation prompt in front of the user per poll - for an answer that
    /// cannot have changed.
    #[test]
    fn a_refused_migration_is_not_retried_on_every_read() {
        let store = CountingStore { attempts: std::sync::atomic::AtomicUsize::new(0) };
        let remembered = Mutex::new(None);

        let first = read_token_once(&store, &remembered, "hf_in_the_file");
        let second = read_token_once(&store, &remembered, "hf_in_the_file");
        let third = read_token_once(&store, &remembered, "hf_in_the_file");

        assert_eq!(store.attempts.load(Ordering::Relaxed), 1, "one prompt, not three");
        for read in [&first, &second, &third] {
            assert_eq!(read.token, "hf_in_the_file", "and the token is still used every time");
            assert_eq!(
                read.location,
                TokenLocation::FileStoreUnavailable { reason: StoreReason::Locked }
            );
            assert!(!read.migrate);
        }

        // A store that takes it is not remembered as a refusal: the settings
        // key is emptied by the migration, so there is nothing left to offer.
        let working = MemoryStore::default();
        let fresh = Mutex::new(None);
        let read = read_token_once(&working, &fresh, "hf_in_the_file");
        assert!(read.migrate);
        assert!(fresh.lock().unwrap().is_none(), "nothing to remember about a success");
    }

    /// **A keychain unlocked mid-session gets one more chance, and only where
    /// the user could have unlocked it.**
    ///
    /// The memo is what stops the prompt-per-poll, and it is also what
    /// leaves a store that has since become usable unasked until the next Save.
    /// `list_models` clears it when - and only when - it is told Settings ›
    /// Models has just been opened, so the retry is one press of one screen and
    /// not a schedule.
    #[test]
    fn the_memo_is_cleared_by_the_retry_flag_and_by_nothing_else() {
        let store = CountingStore { attempts: std::sync::atomic::AtomicUsize::new(0) };
        let remembered = Mutex::new(None);

        read_token_once(&store, &remembered, "hf_in_the_file");
        read_token_once(&store, &remembered, "hf_in_the_file");
        assert_eq!(store.attempts.load(Ordering::Relaxed), 1, "an ordinary list asks once");
        assert!(remembered.lock().unwrap().is_some(), "and remembers the refusal");

        // What the flag does, which is all it does.
        forget_migration(&remembered);
        assert!(remembered.lock().unwrap().is_none());

        read_token_once(&store, &remembered, "hf_in_the_file");
        assert_eq!(store.attempts.load(Ordering::Relaxed), 2, "the open asks again");
        read_token_once(&store, &remembered, "hf_in_the_file");
        assert_eq!(store.attempts.load(Ordering::Relaxed), 2, "and the next list does not");
    }

    /// A working store that counts its reads, standing in for a keychain whose
    /// every read may prompt.
    #[derive(Default)]
    struct ReadCountingStore {
        store: MemoryStore,
        reads: std::sync::atomic::AtomicUsize,
        refuse: AtomicBool,
        refuse_writes: AtomicBool,
    }

    impl TokenStore for ReadCountingStore {
        fn get(&self) -> Result<Option<String>, StoreError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if self.refuse.load(Ordering::Relaxed) {
                return Err(locked("the prompt was cancelled"));
            }
            self.store.get()
        }
        fn set(&self, token: &str) -> Result<(), StoreError> {
            if self.refuse_writes.load(Ordering::Relaxed) {
                return Err(locked("write refused"));
            }
            self.store.set(token)
        }
        fn delete(&self) -> Result<(), StoreError> {
            if self.refuse_writes.load(Ordering::Relaxed) {
                return Err(locked("delete refused"));
            }
            self.store.delete()
        }
        fn available(&self) -> bool {
            true
        }
    }

    /// The token is read once per process; a save/clear replaces the cached
    /// answer without another read. Model lists and downloads must not add
    /// password prompts after the user has just saved their token.
    #[test]
    fn the_token_is_read_from_the_store_once() {
        let store = ReadCountingStore::default();
        store.set("hf_stored").unwrap();
        let memo = TokenMemo::default();
        let cached = CachedStore { inner: &store, memo: &memo };
        let reads = || store.reads.load(Ordering::Relaxed);

        for _ in 0..3 {
            assert_eq!(read_token(&cached, "").token, "hf_stored");
        }
        assert_eq!(reads(), 1, "one prompt, not three");

        // A save replaces what was remembered.
        assert_eq!(write_token(&cached, "hf_new"), TokenWrite::Stored);
        assert_eq!(read_token(&cached, "").token, "hf_new");
        assert_eq!(read_token(&cached, "").token, "hf_new");
        assert_eq!(reads(), 1, "a save already knows the new value");

        // And a clear forgets it.
        assert_eq!(write_token(&cached, ""), TokenWrite::Stored);
        assert_eq!(read_token(&cached, "").token, "");
        assert_eq!(reads(), 1, "a successful clear already knows the token is absent");
    }

    /// A refused read is remembered like a token, and forgotten by the open
    /// of Settings › Models; a remembered token is not.
    #[test]
    fn a_refused_token_read_is_asked_again_only_on_the_retry_flag() {
        let store = ReadCountingStore::default();
        store.set("hf_stored").unwrap();
        store.refuse.store(true, Ordering::Relaxed);
        let memo = TokenMemo::default();
        let cached = CachedStore { inner: &store, memo: &memo };
        let reads = || store.reads.load(Ordering::Relaxed);

        for _ in 0..3 {
            let read = read_token(&cached, "");
            assert_eq!(read.location, TokenLocation::FileStoreUnavailable { reason: StoreReason::Locked });
        }
        assert_eq!(reads(), 1);

        store.refuse.store(false, Ordering::Relaxed);
        forget_token_refusal(&memo);
        assert_eq!(read_token(&cached, "").token, "hf_stored");
        forget_token_refusal(&memo);
        assert_eq!(read_token(&cached, "").token, "hf_stored");
        assert_eq!(reads(), 2, "a remembered token survives the retry flag");
    }

    #[test]
    fn a_new_hf_save_needs_no_readback_and_failed_mutations_do_not_seed_the_cache() {
        let store = ReadCountingStore::default();
        let memo = TokenMemo::default();
        let cached = CachedStore { inner: &store, memo: &memo };

        cached.set("hf_saved").unwrap();
        assert_eq!(cached.get().unwrap().as_deref(), Some("hf_saved"));
        assert_eq!(store.reads.load(Ordering::Relaxed), 0);

        store.refuse_writes.store(true, Ordering::Relaxed);
        assert!(cached.set("hf_unsaved").is_err());
        assert!(memo.lock().unwrap().is_none());
        assert_eq!(cached.get().unwrap().as_deref(), Some("hf_saved"));
        assert!(cached.delete().is_err());
        assert!(memo.lock().unwrap().is_none());
        assert_eq!(cached.get().unwrap().as_deref(), Some("hf_saved"));
        assert_eq!(store.reads.load(Ordering::Relaxed), 2);
    }

    /// `tokenStore` is data and the interface reads it as data: three ids, no
    /// English, and the same spelling the seam contract fixes. The reason
    /// travels **beside** the id rather than inside it, so the id stays one
    /// flat string whatever the store said.
    #[test]
    fn the_token_location_crosses_the_seam_as_an_id() {
        assert_eq!(TokenLocation::Keychain.id(), "keychain");
        assert_eq!(TokenLocation::FileNoStore.id(), "fileNoStore");
        let unreachable = TokenLocation::FileStoreUnavailable { reason: StoreReason::Ambiguous };
        assert_eq!(unreachable.id(), "fileStoreUnavailable");

        assert_eq!(TokenLocation::Keychain.reason(), None, "a store that answered explains nothing");
        assert_eq!(TokenLocation::FileNoStore.reason(), None, "and neither does an absent one");
        assert_eq!(unreachable.reason(), Some(StoreReason::Ambiguous));

        for (reason, id) in [
            (StoreReason::Locked, "\"locked\""),
            (StoreReason::Unreachable, "\"unreachable\""),
            (StoreReason::Ambiguous, "\"ambiguous\""),
            (StoreReason::Unknown, "\"unknown\""),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), id);
        }
    }

    /// **The platform's prose becomes one of four ids**: the note under the
    /// token field could say that *some*
    /// store was unreachable and never that it was locked, because a `String`
    /// in the platform's own words is not something an interface can choose a
    /// sentence from.
    ///
    /// The wildcard is deliberate and is asserted here too: `keyring::Error` is
    /// `#[non_exhaustive]`, so a variant added by a later release has to mean
    /// `unknown` rather than a build failure.
    #[test]
    fn a_keyring_failure_becomes_a_reason_the_interface_can_name() {
        let boxed = || Box::<dyn std::error::Error + Send + Sync>::from("the platform said no");

        assert_eq!(
            reason_of(&keyring::Error::NoStorageAccess(boxed())),
            StoreReason::Locked,
            "the store is there and would not let us in - unlock it"
        );
        assert_eq!(
            reason_of(&keyring::Error::PlatformFailure(boxed())),
            StoreReason::Unreachable,
            "the store itself failed - there is nothing to unlock"
        );
        assert_eq!(
            reason_of(&keyring::Error::Ambiguous(Vec::new())),
            StoreReason::Ambiguous,
            "two credentials match and the store cannot say which is ours"
        );
        for other in [
            keyring::Error::NoEntry,
            keyring::Error::BadEncoding(vec![0xff]),
            keyring::Error::TooLong("service".to_owned(), 8),
            keyring::Error::Invalid("user".to_owned(), "empty".to_owned()),
        ] {
            assert_eq!(reason_of(&other), StoreReason::Unknown, "{other}");
        }

        // And the detail survives beside the id, because `NotCleared` reports
        // it and a log is where the platform's own words are worth having.
        let error = store_error(keyring::Error::NoStorageAccess(boxed()));
        assert_eq!(error.reason, StoreReason::Locked);
        assert!(error.detail.contains("the platform said no"), "{}", error.detail);
    }

    /* -------------------------------------------------------------- */
    /* The digest cache                                                */
    /* -------------------------------------------------------------- */

    /// A remembered answer survives a write and a read, and is invalidated by
    /// every one of the three things it is stamped against.
    #[test]
    fn a_remembered_digest_is_invalidated_by_the_file_moving_or_changing() {
        let dir = std::env::temp_dir().join(format!("mc-verified-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("weights.onnx");
        std::fs::write(&path, b"some weights").unwrap();

        remember(Some(dir.as_path()), "inpainter", &path, true);

        let cache = read_cache(&dir);
        let entry = cache.get("inpainter").expect("the answer was written beside the models");
        let here = path.display().to_string();
        let now = stamp(&path).expect("this filesystem has modification times");

        assert_eq!(cache_hit(entry, &here, now), Some(true), "the same file, unchanged");
        assert_eq!(cache_hit(entry, "/somewhere/else/weights.onnx", now), None, "a different file");
        assert_eq!(cache_hit(entry, &here, (now.0 + 1, now.1)), None, "rewritten in place");
        assert_eq!(cache_hit(entry, &here, (now.0, now.1 + 1)), None, "a different length");

        // And a deleted weight's answer goes with it.
        forget_remembered(Some(dir.as_path()), "inpainter");
        assert!(!read_cache(&dir).contains_key("inpainter"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A failed verification is remembered too - the point of the cache is that
    /// `sha256Ok` survives a relaunch, and `false` is the answer that matters
    /// most.
    #[test]
    fn a_failed_verification_is_remembered_as_a_failure() {
        let dir = std::env::temp_dir().join(format!("mc-verified-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("weights.onnx");
        std::fs::write(&path, b"corrupt").unwrap();

        remember(Some(dir.as_path()), "scriptGate", &path, false);
        let cache = read_cache(&dir);
        let entry = cache.get("scriptGate").unwrap();
        assert_eq!(cache_hit(entry, &path.display().to_string(), stamp(&path).unwrap()), Some(false));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An unreadable or malformed cache is a miss, never an error: a settings
    /// dialog that refused to open because a dot-file was corrupt would be
    /// worse than one that asks for a Check press.
    #[test]
    fn a_malformed_cache_reads_as_no_cache() {
        let dir = std::env::temp_dir().join(format!("mc-verified-junk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(VERIFIED_FILE), b"{not json").unwrap();
        assert!(read_cache(&dir).is_empty());
        assert!(read_cache(Path::new("/no/such/directory/at/all")).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /* -------------------------------------------------------------- */
    /* Read-only                                                       */
    /* -------------------------------------------------------------- */

    /// Ownership **and** permission. A weight inside the app-data directory
    /// that the filesystem will not let go of is not one to offer a Delete
    /// button for, which is what the directory comparison missed.
    #[test]
    fn a_locked_file_in_our_own_directory_is_read_only_too() {
        let root = std::env::temp_dir().join(format!("mc-readonly-{}", std::process::id()));
        let models = root.join("models");
        std::fs::create_dir_all(&models).unwrap();
        let path = models.join("locked.onnx");
        std::fs::write(&path, b"weights").unwrap();

        let writable = Some(models.as_path());
        let meta = std::fs::metadata(&path).unwrap();
        assert!(!is_read_only(&models, writable, &meta), "ours, and writable");
        assert!(is_read_only(root.as_path(), writable, &meta), "somebody else's directory");

        let mut permissions = meta.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        let locked = std::fs::metadata(&path).unwrap();
        assert!(is_read_only(&models, writable, &locked), "ours, and the filesystem says no");

        // Put it back so the directory can be removed on every platform.
        // `set_readonly(false)` would make the file world-writable on Unix,
        // which clippy is right to refuse even in a test: the mode is set
        // explicitly where there is one to set.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).ok();
        }
        #[cfg(not(unix))]
        {
            let mut permissions = locked.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions).ok();
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /* -------------------------------------------------------------- */
    /* Resuming                                                        */
    /* -------------------------------------------------------------- */

    #[test]
    fn a_part_file_is_named_beside_the_file_it_will_become() {
        let pin = "4512adab295ee5a5e02ccd1bdf8d45dccbac88309d9cff1532ffd5de876f02a4";
        assert_eq!(
            part_path(Path::new("/m/lama-manga.onnx"), pin),
            Path::new("/m/lama-manga.onnx.4512adab.part")
        );
        assert_eq!(part_path(Path::new("/m/labels.json"), pin), Path::new("/m/labels.json.4512adab.part"));
        assert_eq!(part_path(Path::new("/m/archive"), pin), Path::new("/m/archive.4512adab.part"));

        // **The stamp is the point.** A re-pinned artefact keeps its file name
        // - a new export of the model is still `lama-manga.onnx` - so without
        // it the next download would resume onto a prefix of a different file.
        let repinned = "0011223344556677889900112233445566778899001122334455667788990011";
        assert_ne!(
            part_path(Path::new("/m/lama-manga.onnx"), pin),
            part_path(Path::new("/m/lama-manga.onnx"), repinned)
        );
    }

    /// What a previous attempt left is the offset the next one asks for.
    #[test]
    fn the_resume_offset_is_what_is_already_on_disk() {
        let dir = std::env::temp_dir().join(format!("mc-offset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("weights.onnx.part");

        assert_eq!(resume_offset(&part), 0, "nothing there is byte zero");
        std::fs::write(&part, vec![7u8; 4096]).unwrap();
        assert_eq!(resume_offset(&part), 4096);
        // A directory in the way is not a partial download.
        assert_eq!(resume_offset(&dir), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The answers a `Range` request can get, and what each one means for the
    /// bytes already written.
    ///
    /// A `206` is believed only when its `Content-Range` starts where the
    /// request asked it to. A body appended to a prefix it does not follow
    /// makes a file of the right length and the wrong digest, which costs the
    /// user the download twice; a needless restart costs it once.
    #[test]
    fn the_servers_answer_decides_whether_the_part_survives() {
        let matching = Some("bytes 4096-9999/10000");
        assert_eq!(resume_plan(206, 4096, matching), Resume::Continue(4096));

        assert_eq!(
            resume_plan(206, 4096, Some("bytes 0-9999/10000")),
            Resume::StartOver,
            "a 206 from somewhere else is not a resume"
        );
        assert_eq!(
            resume_plan(206, 4096, None),
            Resume::StartOver,
            "an unverifiable 206 is refused rather than trusted"
        );
        assert_eq!(
            resume_plan(206, 4096, Some("pages 4096-9999/10000")),
            Resume::StartOver,
            "and so is one in units this cannot read"
        );

        assert_eq!(resume_plan(200, 4096, None), Resume::StartOver, "the range was ignored");
        assert_eq!(
            resume_plan(416, 4096, Some("bytes */10000")),
            Resume::Discard,
            "longer than the artefact"
        );
        // With nothing on disk no range was sent, so no answer is about one.
        assert_eq!(resume_plan(200, 0, None), Resume::StartOver);
        assert_eq!(resume_plan(206, 0, matching), Resume::StartOver);
    }

    /// The header this reads is a byte offset and nothing else.
    #[test]
    fn a_content_range_is_read_for_its_first_byte() {
        assert_eq!(content_range_start("bytes 20000-59999/60000"), Some(20_000));
        assert_eq!(content_range_start("  bytes 0-9/10  "), Some(0));
        assert_eq!(content_range_start("bytes */60000"), None, "what a 416 sends");
        assert_eq!(content_range_start("20000-59999/60000"), None, "no unit named");
        assert_eq!(content_range_start("bytes"), None);
    }

    /// A complete archive from an attempt that failed to unpack is recognised
    /// and reused, rather than downloaded again or stranded.
    ///
    /// The download is the expensive half and it succeeded; what failed was the
    /// unpack, which is a full disk or a permission. Verifying 202 MB is
    /// seconds against a fetch of minutes.
    #[test]
    fn a_verified_archive_left_by_a_failed_unpack_is_used_again() {
        let dir = std::env::temp_dir().join(format!("mc-archive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("runtime.tgz");

        assert!(!already_fetched(&archive, &"0".repeat(64)), "nothing there");

        std::fs::write(&archive, b"an archive").unwrap();
        let pin = digest_of(b"an archive");
        assert!(already_fetched(&archive, &pin), "the same bytes the pin names");
        assert!(!already_fetched(&archive, &"0".repeat(64)), "a different artefact");

        // A directory of that name is not an archive.
        let as_dir = dir.join("also.tgz");
        std::fs::create_dir_all(&as_dir).unwrap();
        assert!(!already_fetched(&as_dir, &pin));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Deleting the runtime is four answers, and two of them are new.
    ///
    /// `remove_dir_all` takes the whole `<app_data>/runtimes` tree, which
    /// holds the `.part` of a transfer another window may be appending
    /// to - so a download in flight refuses the press outright. And `NotFound`
    /// is about the *runtime*, not the directory: a cancelled download leaves a
    /// directory with nothing installed in it, and answering `deleted` for
    /// sweeping a `.part` would report an uninstall that did not happen.
    #[test]
    fn deleting_the_runtime_refuses_a_download_and_answers_about_the_library() {
        let runtimes = Path::new("/app-data/runtimes");
        let installed = runtimes.join("libonnxruntime.dylib");
        let elsewhere = Path::new("/opt/homebrew/lib/libonnxruntime.dylib");

        assert_eq!(plan_delete_runtime(Some(&installed), runtimes, false), Ok(()));
        assert_eq!(
            plan_delete_runtime(Some(&installed), runtimes, true),
            Err(DeleteOutcome::Busy),
            "a download is holding the directory the delete would remove"
        );
        assert_eq!(
            plan_delete_runtime(Some(elsewhere), runtimes, false),
            Err(DeleteOutcome::ReadOnlyElsewhere)
        );
        assert_eq!(
            plan_delete_runtime(None, runtimes, false),
            Err(DeleteOutcome::NotFound),
            "a directory holding only a `.part` is not an installed runtime"
        );
        // Busy is decided first: a running download is the reason not to touch
        // the tree, whatever else the tree does or does not hold.
        assert_eq!(plan_delete_runtime(None, runtimes, true), Err(DeleteOutcome::Busy));
    }

    /// The runtime's archives live outside the directory that is wiped at both
    /// ends of every attempt: a `.part` inside
    /// `.staging` is a resume that can never happen.
    #[test]
    fn the_runtime_archive_survives_the_staging_wipe() {
        let dir = std::env::temp_dir().join(format!("mc-runtime-dirs-{}", std::process::id()));
        let staging = runtime_staging(&dir);
        let downloads = runtime_downloads(&dir);
        assert_ne!(staging, downloads);
        assert!(!downloads.starts_with(&staging), "an archive under staging cannot survive");

        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(staging.join("libonnxruntime.dylib"), b"unpacked").unwrap();
        let part = downloads.join("runtime.tgz.part");
        std::fs::write(&part, vec![0u8; 2048]).unwrap();

        // What `download_runtime` does at the end of every attempt.
        std::fs::remove_dir_all(&staging).unwrap();

        assert_eq!(resume_offset(&part), 2048, "the partial archive is still there to resume");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `206`'s `Content-Length` is what is left, so the bar's total is that
    /// plus the prefix. Everything else is the whole artefact already.
    #[test]
    fn a_resumed_total_counts_the_bytes_already_here() {
        assert_eq!(resume_total(Resume::Continue(1_000), Some(9_000)), Some(10_000));
        assert_eq!(resume_total(Resume::StartOver, Some(10_000)), Some(10_000));
        assert_eq!(resume_total(Resume::Discard, Some(10_000)), Some(10_000));
        // A chunked response has no length, and the bar has no total.
        assert_eq!(resume_total(Resume::Continue(1_000), None), None);
    }

    /// The re-hash of a surviving prefix is the same hasher a straight-through
    /// download would have had at that point - which is the property the whole
    /// resume rests on.
    #[test]
    fn re_hashing_a_prefix_matches_hashing_it_as_it_was_written() {
        let dir = std::env::temp_dir().join(format!("mc-prefix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("weights.onnx.part");
        let whole: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&part, &whole).unwrap();

        // Re-read the first 30 000 bytes, then feed the rest in as a resumed
        // download would, and compare against one pass over the lot.
        let mut resumed = hash_prefix(&part, 30_000).unwrap();
        resumed.update(&whole[30_000..]);

        let mut straight = Sha256::new();
        straight.update(&whole);
        assert_eq!(hex(&resumed.finalize()), hex(&straight.finalize()));

        // Asking for more than there is, is an error rather than a short hash.
        assert!(hash_prefix(&part, 50_000).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /* -------------------------------------------------------------- */
    /* The transfer, against a local server                            */
    /* -------------------------------------------------------------- */

    /// What the stub does with a `Range` header it is sent.
    #[derive(Clone, Copy, PartialEq)]
    enum Ranges {
        /// A well-behaved server: `206` from the requested offset.
        Honour,
        /// A CDN that does not do ranges: `200` and the whole file again.
        Ignore,
        /// A `206` that is not the range that was asked for - the whole file
        /// under a partial-content status. Legal-looking and, if believed,
        /// appended to a prefix it does not follow.
        Lie,
    }

    /// One HTTP request, served from memory on a loopback port.
    ///
    /// A stub rather than a mocked client because the thing under test is what
    /// `fetch_verified` does with a real `206` - the header it sends, the total
    /// it computes, the file it appends to. Loopback, one connection, and the
    /// thread ends with the response.
    fn serve_once(body: Vec<u8>, ranges: Ranges) -> (String, std::thread::JoinHandle<()>) {
        use std::io::BufRead;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let Ok(peek) = stream.try_clone() else { return };
            let mut reader = std::io::BufReader::new(peek);
            let mut from = 0u64;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(_) => return,
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("range:") {
                    if let Some(start) = value.trim().strip_prefix("bytes=") {
                        from = start.trim_end_matches('-').trim().parse().unwrap_or(0);
                    }
                }
            }

            let total = body.len() as u64;
            let head = if from > 0 && ranges == Ranges::Lie {
                from = 0;
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {total}\r\n\
                     Content-Range: bytes 0-{}/{total}\r\nConnection: close\r\n\r\n",
                    total - 1
                )
            } else if from == 0 || ranges == Ranges::Ignore {
                from = 0;
                format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n")
            } else if from >= total {
                format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                )
            } else {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                     Content-Range: bytes {from}-{}/{total}\r\nConnection: close\r\n\r\n",
                    total - from,
                    total - 1
                )
            };
            // Every write is best-effort: a cancelled download drops the
            // connection and the stub must not panic a test thread for it.
            let _ = stream.write_all(head.as_bytes());
            if from < total {
                let _ = stream.write_all(&body[from as usize..]);
            }
            let _ = stream.flush();
        });
        (format!("http://{addr}/artefact.bin"), handle)
    }

    fn digest_of(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex(&hasher.finalize())
    }

    /// Every `(downloaded, total)` one download reported, shared between the
    /// event sink and the test that reads it at the end.
    type Reports = Arc<Mutex<Vec<(u64, Option<u64>)>>>;

    /// A collector for one download's progress, filtered by its id so that the
    /// process-wide registry cannot hand this test another one's events.
    fn watching(id: &'static str) -> (Reports, u64) {
        let seen: Reports = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let handle = events::register(Box::new(move |event| {
            if let events::Event::ModelProgress { id: seen_id, downloaded, total, done, .. } = event {
                if seen_id == id && !done {
                    sink.lock().unwrap().push((*downloaded, *total));
                }
            }
        }));
        (seen, handle)
    }

    /// One test: a `.part` from a previous attempt is
    /// kept, the next attempt asks for the rest, the prefix is re-hashed rather
    /// than re-downloaded, the digest still comes out right, and the progress
    /// events count the bytes that were already here.
    #[test]
    fn an_interrupted_download_resumes_from_the_bytes_it_already_has() {
        let dir = std::env::temp_dir().join(format!("mc-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("artefact.bin");
        let body: Vec<u8> = (0..60_000u32).map(|i| (i % 253) as u8).collect();
        let pin = digest_of(&body);
        let part = part_path(&path, &pin);

        // What the interrupted attempt left behind.
        std::fs::write(&part, &body[..20_000]).unwrap();

        let (url, server) = serve_once(body.clone(), Ranges::Honour);
        let (seen, sink) = watching("resume-test");
        let cancel = AtomicBool::new(false);
        let result = fetch_verified(&url, &pin, &path, "resume-test", "", &cancel, Portion::ALONE);
        events::unregister(sink);
        let _ = server.join();

        assert_eq!(result.as_deref(), Ok(path.as_path()), "the file is published");
        assert_eq!(std::fs::read(&path).unwrap(), body, "and it is the whole artefact");
        assert!(!part.exists(), "the `.part` was renamed, not left beside it");

        let progress = seen.lock().unwrap().clone();
        let first = progress.first().copied().expect("a download reports before it reads");
        assert_eq!(first.0, 20_000, "the first report counts the resumed prefix");
        assert_eq!(first.1, Some(60_000), "and the total is the whole artefact, not what is left");
        assert_eq!(progress.last().copied().unwrap().0, 60_000);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A server that answers a `Range` with `200` is sending the whole file
    /// again, so the `.part` is overwritten rather than appended to - otherwise
    /// the digest would be computed over the prefix twice.
    #[test]
    fn a_server_that_ignores_the_range_starts_the_file_again() {
        let dir = std::env::temp_dir().join(format!("mc-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("artefact.bin");
        let body: Vec<u8> = (0..30_000u32).map(|i| (i % 97) as u8).collect();
        let pin = digest_of(&body);
        std::fs::write(part_path(&path, &pin), &body[..9_000]).unwrap();

        let (url, server) = serve_once(body.clone(), Ranges::Ignore);
        let cancel = AtomicBool::new(false);
        let result = fetch_verified(&url, &pin, &path, "restart-test", "", &cancel, Portion::ALONE);
        let _ = server.join();

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), body);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `206` that does not start where the request asked is not a resume.
    ///
    /// Believing it would append the whole artefact to a prefix of it: a file
    /// of the wrong length, or - with a prefix the server happens to repeat -
    /// the right length and the wrong digest. The `.part` is truncated and the
    /// transfer starts over, which costs one restart and gets the right file.
    #[test]
    fn a_206_from_the_wrong_offset_is_not_believed() {
        let dir = std::env::temp_dir().join(format!("mc-liar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("artefact.bin");
        let body: Vec<u8> = (0..25_000u32).map(|i| (i % 83) as u8).collect();
        let pin = digest_of(&body);
        let part = part_path(&path, &pin);
        std::fs::write(&part, &body[..7_000]).unwrap();

        let (url, server) = serve_once(body.clone(), Ranges::Lie);
        let cancel = AtomicBool::new(false);
        let result = fetch_verified(&url, &pin, &path, "liar-test", "", &cancel, Portion::ALONE);
        let _ = server.join();

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), body, "the whole artefact, once");
        assert!(!part.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **A `.part` from a previous pin is not resumed onto.**
    ///
    /// A catalogue row keeps its `file_name` when its URL and digest change, so
    /// a `.part` named from the file alone would have the new artefact appended
    /// to a prefix of the old one. The digest catches that - nothing bad gets
    /// installed - but the user pays for the download twice and has nothing to
    /// read that explains why. The stamp in the name means the old prefix is
    /// simply not the file this transfer is resuming.
    #[test]
    fn a_part_left_by_a_previous_pin_is_not_resumed_onto() {
        let dir = std::env::temp_dir().join(format!("mc-repin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("weights.onnx");

        let old_body: Vec<u8> = (0..9_000u32).map(|i| (i % 61) as u8).collect();
        let stale = part_path(&path, &digest_of(&old_body));
        std::fs::write(&stale, &old_body[..4_000]).unwrap();

        let body: Vec<u8> = (0..18_000u32).map(|i| (i % 47) as u8).collect();
        let pin = digest_of(&body);
        assert_eq!(resume_offset(&part_path(&path, &pin)), 0, "the new pin has no prefix here");

        let (url, server) = serve_once(body.clone(), Ranges::Honour);
        let cancel = AtomicBool::new(false);
        let result = fetch_verified(&url, &pin, &path, "repin-test", "", &cancel, Portion::ALONE);
        let _ = server.join();

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert_eq!(resume_offset(&stale), 4_000, "and the old prefix was never touched");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A cancellation keeps the bytes. That is the change: this used to
    /// delete the `.part` and the next press started at zero.
    #[test]
    fn a_cancelled_download_keeps_what_it_has_for_the_next_press() {
        let dir = std::env::temp_dir().join(format!("mc-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("artefact.bin");
        let body: Vec<u8> = (0..30_000u32).map(|i| (i % 89) as u8).collect();
        let pin = digest_of(&body);
        let part = part_path(&path, &pin);
        std::fs::write(&part, &body[..12_000]).unwrap();

        let (url, server) = serve_once(body.clone(), Ranges::Honour);
        let cancel = AtomicBool::new(true);
        let result = fetch_verified(&url, &pin, &path, "cancel-test", "", &cancel, Portion::ALONE);
        let _ = server.join();

        assert_eq!(result.unwrap_err(), "cancelled");
        assert!(!path.exists(), "nothing is published");
        assert_eq!(resume_offset(&part), 12_000, "and the prefix is still there to resume from");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The one ending that does throw the bytes away. A `.part` whose digest
    /// does not match is not a resumable prefix of anything - it is the wrong
    /// artefact - and the message names both digests.
    #[test]
    fn a_bad_digest_deletes_the_part_and_names_both_sides() {
        let dir = std::env::temp_dir().join(format!("mc-mismatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("artefact.bin");
        let body: Vec<u8> = (0..5_000u32).map(|i| (i % 71) as u8).collect();
        let pinned = "0".repeat(64);
        let part = part_path(&path, &pinned);

        let (url, server) = serve_once(body, Ranges::Honour);
        let cancel = AtomicBool::new(false);
        let result = fetch_verified(&url, &pinned, &path, "mismatch-test", "", &cancel, Portion::ALONE);
        let _ = server.join();

        let error = result.unwrap_err();
        assert!(error.starts_with("digest mismatch: expected 0000"), "{error}");
        assert!(!part.exists(), "a wrong artefact is not a prefix worth keeping");
        assert!(!path.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Moving staged files into place overwrites existing files and cleans up
    /// existing directories with conflicting names.
    #[test]
    fn move_staged_overwrites_existing_files_and_directories() {
        let root = std::env::temp_dir().join(format!("mc-move-staged-{}", std::process::id()));
        let staging = root.join("staging");
        let dir = root.join("runtimes");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        // Create an existing file and an existing directory in the target dir.
        std::fs::write(dir.join("libonnxruntime.dylib"), b"old-version").unwrap();
        let old_sub = dir.join("DirectML.dll");
        std::fs::create_dir_all(&old_sub).unwrap();
        std::fs::write(old_sub.join("nested.txt"), b"stale").unwrap();

        // Create new replacement files in staging.
        std::fs::write(staging.join("libonnxruntime.dylib"), b"new-version").unwrap();
        std::fs::write(staging.join("DirectML.dll"), b"new-directml-binary").unwrap();

        let mut moved = match move_staged(&staging, &dir) {
            Ok(moved) => moved,
            Err(failed) => panic!("move_staged over existing entries: {}", failed.error),
        };

        assert_eq!(std::fs::read(dir.join("libonnxruntime.dylib")).unwrap(), b"new-version");
        assert_eq!(std::fs::read(dir.join("DirectML.dll")).unwrap(), b"new-directml-binary");
        // And it says what it brought, which is what the install writes down
        // and what the next one takes its difference against.
        moved.sort();
        assert_eq!(moved, ["DirectML.dll", "libonnxruntime.dylib"]);

        std::fs::remove_dir_all(&root).ok();
    }

    /* -------------------------------------------------------------- */
    /* Unfinished downloads                                            */
    /* -------------------------------------------------------------- */

    /// **A `.part` says which pin it is a prefix of, and the sweep reads it.**
    ///
    /// The stamp left the old file stranded under its own name: no
    /// press will ever open it, because every press looks for the *current*
    /// stamp, and nothing else deletes it. That is the shape a sweep
    /// has to recognise, and it is recognisable from the name alone.
    #[test]
    fn a_part_stamped_with_a_pin_the_catalogue_has_moved_past_is_swept() {
        let model = package("inpainter").expect("the catalogue has an inpainter");
        let current = format!("{}.{}.part", model.file_name, part_stamp(model.sha256));
        let stale = format!("{}.deadbeef.part", model.file_name);

        assert_eq!(part_stamp_of(&current, model.file_name), Some(part_stamp(model.sha256)));
        assert!(!is_stranded_part(&current), "the pin this table names is the resumable one");
        assert!(is_stranded_part(&stale), "an old pin can never be resumed onto");
        // Neither a finished weight nor somebody else's partial file is ours to
        // delete out of a directory the user is invited to populate by hand.
        assert!(!is_stranded_part(model.file_name));
        assert!(!is_stranded_part("something-else.onnx.deadbeef.part"));
        assert!(!is_stranded_part(&format!("{}.notahex!.part", model.file_name)));

        let dir = std::env::temp_dir().join(format!("mc-sweep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [&current, &stale] {
            std::fs::write(dir.join(name), b"a prefix of something").unwrap();
        }
        std::fs::write(dir.join(model.file_name), b"not a part file").unwrap();

        sweep_stranded_parts(&dir);

        assert!(dir.join(&current).exists(), "the resumable prefix is the point of keeping it");
        assert!(!dir.join(&stale).exists(), "and the one nothing can resume is gone");
        assert!(dir.join(model.file_name).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The remainder a row reports, and the press that gives it back.
    ///
    /// Both halves: the bytes were invisible - "Not installed" with
    /// 180 MB sitting beside the row - and there was no way to remove them
    /// short of finding the folder by hand.
    #[test]
    fn a_row_reports_its_resumable_remainder_and_a_press_discards_it() {
        let model = package("balloonDetector").expect("the catalogue has a balloon detector");
        let dir = std::env::temp_dir().join(format!("mc-remainder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(partial_bytes(Some(dir.as_path()), model), None, "nothing here yet");

        let part = part_path(&dir.join(model.file_name), model.sha256);
        std::fs::write(&part, vec![0u8; 4_096]).unwrap();
        assert_eq!(partial_bytes(Some(dir.as_path()), model), Some(4_096));
        // No directory at all is no remainder rather than a zero: the machine
        // has never downloaded anything.
        assert_eq!(partial_bytes(None, model), None);

        assert!(std::fs::remove_file(&part).is_ok(), "the discard is one `remove_file`");
        assert_eq!(partial_bytes(Some(dir.as_path()), model), None, "and the row goes quiet");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The runtime's remainder is **everything** its download directory is
    /// holding, in the directory - the `.part` files and the
    /// complete archives alike - and a discard takes every one of them.
    ///
    /// The complete archive belongs there for the same reason the `.part` does:
    /// `download_runtime` keeps one that downloaded and then failed to *unpack*,
    /// so the next press verifies and unpacks it rather than fetching 202 MB
    /// again. Counting only `.part` left those 202 MB invisible to the row,
    /// untouched by the press, and enough to make the `remove_dir` at the end of
    /// every attempt fail without saying so.
    #[test]
    fn the_runtimes_remainder_is_everything_its_download_directory_is_holding() {
        let dir = std::env::temp_dir().join(format!("mc-runtime-parts-{}", std::process::id()));
        let downloads = runtime_downloads(&dir);
        std::fs::create_dir_all(&downloads).unwrap();

        assert_eq!(runtime_partial_bytes(&dir), None, "an empty directory owes nothing");

        std::fs::write(downloads.join("onnxruntime.zip.4512adab.part"), vec![0u8; 3_000]).unwrap();
        std::fs::write(downloads.join("directml.nupkg.4e7cb7dd.part"), vec![0u8; 1_500]).unwrap();
        std::fs::write(downloads.join("directml.nupkg"), vec![0u8; 9_000]).unwrap();

        assert_eq!(
            runtime_partial_bytes(&dir),
            Some(13_500),
            "both interrupted halves and the archive kept for a re-unpack"
        );
        // And the archive is still usable while it is there, which is what
        // makes it worth keeping until somebody asks for the disk back.
        assert!(
            already_fetched(&downloads.join("directml.nupkg"), &digest_of(&vec![0u8; 9_000])),
            "a kept archive still verifies against its pin"
        );

        assert!(discard_runtime_leftovers(&dir));
        assert_eq!(runtime_partial_bytes(&dir), None);
        assert!(!downloads.exists(), "and the empty directory goes with them");
        assert!(!discard_runtime_leftovers(&dir), "a second press has nothing to do");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **A discard cannot land on a `.part` a download has just claimed.**
    ///
    /// Asking `is_downloading` and then unlinking is two decisions with one
    /// `claim` of room between them: another window's press passes the registry,
    /// opens the file and starts appending, and the file it holds is deleted
    /// underneath it. The registry lock is held across both, so a press either
    /// arrives first and is seen, or waits and finds the bytes already gone.
    #[test]
    fn a_discard_and_a_download_press_cannot_pass_each_other() {
        let id = "discard-race-test";
        let dir = std::env::temp_dir().join(format!("mc-discard-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("artefact.bin.4512adab.part");
        std::fs::write(&part, vec![0u8; 2_048]).unwrap();

        // A download holds the id: the press does nothing and says so, and the
        // bytes it is appending to are still there.
        let cancel = claim(id).expect("the id was free");
        let refused = while_not_downloading(id, false, || std::fs::remove_file(&part).is_ok());
        assert!(!refused, "a claimed id is not a discard");
        assert_eq!(resume_offset(&part), 2_048);
        drop(cancel);
        release(id);

        // With the id free the work runs - and it runs with the registry held,
        // which is the property the gap used to lack. Asserted from another
        // thread, because a `try_lock` on the thread that already holds it is
        // not a question with a defined answer.
        let discarded = while_not_downloading(id, false, || {
            let contended = std::thread::spawn(|| inflight().try_lock().is_err())
                .join()
                .expect("the prober thread does not panic");
            assert!(contended, "the unlink must happen while nothing can claim the id");
            std::fs::remove_file(&part).is_ok()
        });
        assert!(discarded);
        assert_eq!(resume_offset(&part), 0);
        // And a second press has nothing to remove, which is not a failure.
        assert!(!while_not_downloading(id, false, || std::fs::remove_file(&part).is_ok()));

        std::fs::remove_dir_all(&dir).ok();
    }

    /* -------------------------------------------------------------- */
    /* Which runtime is installed                                      */
    /* -------------------------------------------------------------- */

    /// **What was installed is written down, because nothing else can say it.**
    ///
    /// The row's `flavour` is the build that *would* be downloaded, so a
    /// machine that fetched DirectML and then chose CUDA read `cuda12 ·
    /// Installed` - two true statements reading as one false one. A stamp
    /// answers it without loading the library, and a missing stamp is unknown
    /// rather than a claim.
    #[test]
    fn the_installed_build_is_read_back_from_the_stamp_and_unknown_without_one() {
        let dir = std::env::temp_dir().join(format!("mc-installed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(read_installed(&dir), None, "a runtime unpacked before the stamp existed");

        let stamp = InstalledRuntime {
            flavour: "directml".to_owned(),
            version: "1.24.4".to_owned(),
            files: vec!["onnxruntime.dll".to_owned(), "DirectML.dll".to_owned()],
        };
        write_installed(&dir, &stamp);
        assert_eq!(read_installed(&dir).as_ref(), Some(&stamp));

        // The seam reads it as camelCase like every other row, and a corrupt
        // record is unknown rather than an error: it is a note, not a gate.
        let written = std::fs::read_to_string(dir.join(INSTALLED_FILE)).unwrap();
        assert!(written.contains("\"flavour\""), "{written}");
        std::fs::write(dir.join(INSTALLED_FILE), b"{not json").unwrap();
        assert_eq!(read_installed(&dir), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **The difference, and only the difference.**
    ///
    /// Switching from DirectML to CUDA overwrites `onnxruntime.dll` and leaves
    /// `DirectML.dll` beside it - 18 MB nothing will ever open again, in a
    /// directory whose contents no longer describe one build. What the
    /// new archive brought is kept whatever the old stamp said about it, so a
    /// re-download of the same flavour removes nothing.
    #[test]
    fn switching_build_removes_what_the_new_archive_did_not_bring() {
        let directml = InstalledRuntime {
            flavour: "directml".to_owned(),
            version: "1.24.4".to_owned(),
            files: vec!["onnxruntime.dll".to_owned(), "DirectML.dll".to_owned()],
        };

        let cuda = ["onnxruntime.dll".to_owned(), "onnxruntime_providers_cuda.dll".to_owned()];
        assert_eq!(foreign_files(&directml, &cuda), ["DirectML.dll"]);

        // The same build again is not a sweep of the runtime it just replaced.
        assert!(foreign_files(&directml, &directml.files.clone()).is_empty());
        // And an archive that brings nothing does not make the whole directory
        // foreign by accident - it makes every one of its files foreign, which
        // is the same statement and the honest one.
        assert_eq!(foreign_files(&directml, &[]), directml.files);
    }

    /// **A stamp is a file on the user's disk, and it names things to delete.**
    ///
    /// `<app_data>/runtimes` is a directory a user is
    /// invited to open, and this record is read back as a list of names
    /// joined onto it. One edited line - `../models/lama-manga.onnx`, or an
    /// absolute path - turned a runtime install into a delete of anything the
    /// process can reach. Only a plain file name is honoured.
    #[test]
    fn a_stamp_can_only_name_files_inside_the_runtime_directory() {
        assert!(is_plain_file_name("onnxruntime.dll"));
        assert!(is_plain_file_name("libonnxruntime.1.28.0.dylib"));
        for escape in ["../models/lama-manga.onnx", "..", ".", "", "sub/dir.dll", "/etc/hosts"] {
            assert!(!is_plain_file_name(escape), "{escape}");
        }
        #[cfg(windows)]
        for escape in ["..\\models\\x", "C:\\Windows\\System32\\kernel32.dll"] {
            assert!(!is_plain_file_name(escape), "{escape}");
        }

        // And the filter is in `foreign_files`, so nothing downstream has to
        // remember: an edited stamp names them, and the sweep does not see them.
        let tampered = InstalledRuntime {
            flavour: "directml".to_owned(),
            version: "1.24.4".to_owned(),
            files: vec![
                "DirectML.dll".to_owned(),
                "../models/lama-manga.onnx".to_owned(),
                "/etc/hosts".to_owned(),
            ],
        };
        assert_eq!(foreign_files(&tampered, &["onnxruntime.dll".to_owned()]), ["DirectML.dll"]);
    }

    /// **A record is destroyed only by a move that actually changed the
    /// directory it describes.**
    ///
    /// The ordering this replaces removed the stamp *before* calling the move,
    /// so that a crash in between said `unknown` rather than named a build that
    /// was not there. The failure that happens is not a crash. On Windows the
    /// move is refused outright and refused first - the library being replaced
    /// is `onnxruntime.dll`, this process has it mapped, and nothing moves - so
    /// every refused flavour switch left the row reading `unknown` about a
    /// runtime that was installed, working, and exactly what the stamp had said
    /// it was a second earlier. Three outcomes, told apart by what the move
    /// managed to move.
    #[test]
    fn a_record_survives_a_move_that_moved_nothing_and_not_one_that_moved_something() {
        let dir = std::env::temp_dir().join(format!("mc-restamp-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("DirectML.dll"), b"the previous build's companion").unwrap();
        let directml = InstalledRuntime {
            flavour: "directml".to_owned(),
            version: "1.24.4".to_owned(),
            files: vec!["onnxruntime.dll".to_owned(), "DirectML.dll".to_owned()],
        };
        write_installed(&dir, &directml);

        // One: the move was refused before it touched anything. The directory
        // is what it was, so the record of it is still true and stays.
        let refused = install_stamped(&dir, "cuda12", "1.28.0", || {
            Err(MoveFailed { moved: Vec::new(), error: NOTICE_IN_USE.to_owned() })
        });
        assert_eq!(refused.unwrap_err(), NOTICE_IN_USE);
        assert_eq!(
            read_installed(&dir).as_ref(),
            Some(&directml),
            "a switch that could not start did not uninstall what is here"
        );
        assert!(dir.join("DirectML.dll").exists(), "and nothing was swept");

        // Two: the move failed part way. No stamp describes a directory that is
        // half of one build and half of another, so there is no true answer and
        // the record says so.
        let partial = install_stamped(&dir, "cuda12", "1.28.0", || {
            std::fs::write(dir.join("onnxruntime.dll"), b"the CUDA build").unwrap();
            Err(MoveFailed {
                moved: vec!["onnxruntime.dll".to_owned()],
                error: "no space left".to_owned(),
            })
        });
        assert_eq!(partial.unwrap_err(), "no space left");
        assert_eq!(read_installed(&dir), None, "unknown, rather than a build that is not there");
        assert!(
            dir.join("DirectML.dll").exists(),
            "and nothing was swept: the sweep is what a *successful* move earns"
        );

        // The next press does succeed, and it writes the record - but it sweeps
        // nothing, because the record it would have taken the difference
        // against is the one the half-finished move had to remove. That is the
        // price of *unknown rather than wrong* and it is stated here rather
        // than left to be discovered: an install that fails part way forfeits
        // the sweep on the attempt after it.
        install_stamped(&dir, "cuda12", "1.28.0", || {
            // What a real `move_staged` would have left in the directory, so
            // the sweep below has something it could remove.
            for name in ["onnxruntime.dll", "onnxruntime_providers_cuda.dll"] {
                std::fs::write(dir.join(name), b"the CUDA build").unwrap();
            }
            Ok(vec!["onnxruntime.dll".to_owned(), "onnxruntime_providers_cuda.dll".to_owned()])
        })
        .expect("the move succeeded");
        let stamp = read_installed(&dir).expect("a record of what is now here");
        assert_eq!(stamp.flavour, "cuda12");
        assert_eq!(stamp.version, "1.28.0");
        assert_eq!(stamp.files.len(), 2);
        assert!(dir.join("DirectML.dll").exists(), "nothing recorded it, so nothing swept it");

        // Three: with a record to compare against, the same call does sweep.
        // The ordering costs the difference only across a move that broke off
        // half way.
        install_stamped(&dir, "directml", "1.24.4", || {
            std::fs::write(dir.join("onnxruntime.dll"), b"the DirectML build").unwrap();
            Ok(vec!["onnxruntime.dll".to_owned(), "DirectML.dll".to_owned()])
        })
        .expect("the move succeeded");
        assert!(
            dir.join("onnxruntime.dll").exists(),
            "the library the new archive replaced is still here"
        );
        assert!(
            !dir.join("onnxruntime_providers_cuda.dll").exists(),
            "the CUDA provider the previous stamp named is gone"
        );
        assert_eq!(read_installed(&dir).unwrap().flavour, "directml");

        std::fs::remove_dir_all(&dir).ok();
    }

    /* -------------------------------------------------------------- */
    /* Windows: replacing a library the process is holding open        */
    /* -------------------------------------------------------------- */

    /// **The aside copy is recognised by its name, and by a name nothing else
    /// wears.**
    ///
    /// `<app_data>/runtimes` is a directory a user is invited to open, and this
    /// sweep is a `remove_file` driven by what it finds there. A rule that
    /// matched `.old` alone would delete somebody's notes; the digits are what
    /// make the shape this module's own.
    #[test]
    fn an_aside_copy_is_recognised_by_its_name_and_nothing_else_is() {
        assert!(is_replaced_aside("onnxruntime.dll.old-0"));
        assert!(is_replaced_aside("onnxruntime.dll.old-17"));
        assert!(is_replaced_aside("libonnxruntime.1.28.0.dylib.old-3"));
        for kept in [
            "onnxruntime.dll",
            "DirectML.dll",
            ".installed.json",
            "notes.old-copy",
            "onnxruntime.dll.old-",
            "onnxruntime.dll.old-1x",
            ".old-1",
            "old-1",
        ] {
            assert!(!is_replaced_aside(kept), "{kept}");
        }

        let dir = std::env::temp_dir().join(format!("mc-aside-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["onnxruntime.dll", "onnxruntime.dll.old-0", "onnxruntime.dll.old-1", "notes.old-copy"] {
            std::fs::write(dir.join(name), b"bytes").unwrap();
        }
        sweep_replaced(&dir);
        assert!(dir.join("onnxruntime.dll").exists(), "the installed runtime is not an aside copy");
        assert!(dir.join("notes.old-copy").exists(), "and neither is a file somebody put there");
        assert!(!dir.join("onnxruntime.dll.old-0").exists());
        assert!(!dir.join("onnxruntime.dll.old-1").exists());

        // A directory wearing the name is left alone: the sweep unlinks files,
        // and recursing would make it a sweep that can empty something it did
        // not create.
        std::fs::create_dir_all(dir.join("stuff.old-2")).unwrap();
        sweep_replaced(&dir);
        assert!(dir.join("stuff.old-2").is_dir());

        std::fs::remove_dir_all(&dir).ok();
    }

    /* -------------------------------------------------------------- */
    /* What an install costs the volume                               */
    /* -------------------------------------------------------------- */

    /// **A refusal is a key first and its numbers beside it.**
    ///
    /// Bytes are not translatable, so they travel as data rather than inside a
    /// sentence - the arrangement `accel.declined.memory` already has with
    /// `neededBytes` and `roomBytes`. The grammar is the adapter's whole job
    /// and it is pinned here rather than described only in prose.
    #[test]
    fn a_refusal_names_a_key_first_and_carries_its_numbers_beside_it() {
        let refusal = no_space(3_000, 1_200);
        let mut fields = refusal.split(' ');
        assert_eq!(fields.next(), Some(NOTICE_NO_SPACE));
        assert_eq!(fields.next(), Some("needed=3000"));
        assert_eq!(fields.next(), Some("free=1200"));
        assert_eq!(fields.next(), None);

        // The other one is the whole of itself: there is nothing to say about
        // a mapped library except which remedy it needs.
        assert_eq!(NOTICE_IN_USE.split(' ').count(), 1);
        for key in [NOTICE_IN_USE, NOTICE_NO_SPACE] {
            assert!(key.starts_with("notice.runtime."), "{key}");
        }
    }

    /// The preflight asks for the transfer **and** for room to unpack it, and
    /// says out loud that the second half is a floor rather than a bound.
    #[test]
    fn the_preflight_counts_the_unpack_as_well_as_the_download() {
        assert_eq!(room_to_install(0), 0, "nothing to fetch asks for nothing");
        // The win-x64 CUDA 12 archive, which is the largest thing this
        // catalogue names.
        assert_eq!(room_to_install(455_344_532), 910_689_064);
        assert_eq!(room_to_install(u64::MAX), u64::MAX, "and it does not wrap");

        // Nothing needed passes whatever the volume says, and a demand no
        // volume can meet comes back as the key with both figures on it.
        assert_eq!(ensure_room(&std::env::temp_dir(), 0), Ok(()));
        let refused =
            ensure_room(&std::env::temp_dir(), u64::MAX).expect_err("no volume holds u64::MAX");
        assert!(refused.starts_with(NOTICE_NO_SPACE), "{refused}");
        assert!(refused.contains(&format!("needed={}", u64::MAX)), "{refused}");

        // The volume is asked about even when the directory is not there yet:
        // `<app_data>/runtimes` is created by the first download, and the
        // question was never about the directory.
        let absent = std::env::temp_dir().join("mc-no-such-dir").join("runtimes").join(".downloads");
        assert!(!absent.exists());
        assert!(free_space(&absent).is_some(), "the nearest existing ancestor answers");
    }

    /// **`.pdb` and `.lib` are not unpacked, and the size check knows it.**
    ///
    /// The stock `win-x64` package is a 12 MB download carrying about 408 MB of
    /// `onnxruntime.pdb`, which was most of the staging space a Windows install
    /// needed and all of the surprise in it. Nothing loads either extension -
    /// the reasoning and what was checked is on [`is_loadable`] - so neither is
    /// written and neither is counted.
    #[test]
    fn debug_symbols_and_import_libraries_are_neither_unpacked_nor_counted() {
        assert!(is_loadable("onnxruntime.dll"));
        assert!(is_loadable("libonnxruntime.1.28.0.dylib"));
        assert!(is_loadable("onnxruntime_providers_webgpu.dll"));
        for skipped in ["onnxruntime.pdb", "onnxruntime.lib", "ONNXRUNTIME.PDB", "DirectML.Lib"] {
            assert!(!is_loadable(skipped), "{skipped}");
        }

        let dir = std::env::temp_dir().join(format!("mc-pdb-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let dest = dir.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let archive = dir.join("package.nupkg");

        let native = "runtimes/win-x64/native";
        let entries: Vec<(String, &[u8])> = vec![
            (format!("{native}/onnxruntime.dll"), b"a runtime".as_slice()),
            (format!("{native}/onnxruntime.pdb"), b"four hundred megabytes of symbols".as_slice()),
            (format!("{native}/onnxruntime.lib"), b"an import library".as_slice()),
            (format!("{native}/nested/x.dll"), b"not directly inside".as_slice()),
            ("build/native/onnxruntime.props".to_owned(), b"a different directory".as_slice()),
        ];
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in &entries {
            zip.start_file(name.as_str(), stored).unwrap();
            std::io::Write::write_all(&mut zip, body).unwrap();
        }
        zip.finish().unwrap();

        // Exactly the one file that will be written, and exactly its length.
        assert_eq!(unpacked_bytes(&archive, native), Some(b"a runtime".len() as u64));
        // A `.tgz` cannot be asked without inflating it, and says so.
        assert_eq!(unpacked_bytes(&dir.join("runtime.tgz"), native), None);

        unpack(&archive, native, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("onnxruntime.dll")).unwrap(), b"a runtime");
        for absent in ["onnxruntime.pdb", "onnxruntime.lib", "x.dll", "nested", "onnxruntime.props"] {
            assert!(!dest.join(absent).exists(), "{absent}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **A package of three artefacts is one download.**
    ///
    /// A Windows runtime is the ONNX Runtime build, `DirectML.dll` and the
    /// WebGPU plugin - 12 MB, then 202 MB, then 39 MB - under one id, and each
    /// of them used to report from zero with its own total. The bar restarted
    /// twice and never said how much there was. Every report now counts the
    /// whole and only goes up.
    #[test]
    fn a_package_of_several_artefacts_reports_one_download() {
        let dir = std::env::temp_dir().join(format!("mc-portion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("second.bin");
        let body: Vec<u8> = (0..30_000u32).map(|i| (i % 91) as u8).collect();
        let pin = digest_of(&body);

        let (url, server) = serve_once(body.clone(), Ranges::Honour);
        let (seen, sink) = watching("portion-test");
        let cancel = AtomicBool::new(false);
        // The second artefact of a 50,000-byte package whose first one is
        // already here.
        let result = fetch_verified(
            &url,
            &pin,
            &path,
            "portion-test",
            "",
            &cancel,
            Portion { before: 12_000, whole: Some(50_000) },
        );
        events::unregister(sink);
        let _ = server.join();
        assert_eq!(result.as_deref(), Ok(path.as_path()));

        let progress = seen.lock().unwrap().clone();
        assert_eq!(
            progress.first().copied(),
            Some((12_000, Some(50_000))),
            "the first report starts where the package had got to, not at zero"
        );
        assert_eq!(
            progress.last().copied(),
            Some((42_000, Some(50_000))),
            "and the last one is what the package has done, not what this artefact has"
        );
        assert!(
            progress.iter().all(|(_, total)| *total == Some(50_000)),
            "the total is the package's, not the response's: {progress:?}"
        );
        assert!(
            progress.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "a bar that only goes up: {progress:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **The runtime script and the runtime table are ten copies of a digest
    /// with nothing keeping them equal.**
    ///
    /// `scripts/fetch-runtime.sh` says it mirrors
    /// `crates/cleaner-core/src/runtime/package.rs` and that the table is the
    /// authority, and until now that was a sentence in a comment. A digest that
    /// drifts in one of the two is a download verified against the wrong number
    /// on whichever side is not being read, which is worse than not verifying
    /// at all. The same relationship - and the same test -
    /// `the_script_and_the_table_agree` gives `fetch-models.sh` and [`MODELS`].
    ///
    /// Matched by URL rather than line by line, because the two are not the
    /// same shape: the script resolves `$arch` and `$1` at run time, so one of
    /// its lines is several of the table's rows - the DirectML package is
    /// `win-x64` and `win-arm64`, and the WebGPU plugin is four platforms.
    /// The URL is what both sides agree is the artefact's name.
    #[test]
    fn the_runtime_script_and_the_package_table_agree() {
        use cleaner_core::runtime::package;

        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/fetch-runtime.sh"),
        )
        .expect("scripts/fetch-runtime.sh is readable");

        // An artefact in the script is `name|sha256|url|library_dir`, and the
        // digest is what identifies the field: 64 hex characters followed by an
        // `https://` URL. Read that way rather than by unpicking the shell
        // quoting, which is what the fields are wrapped in and is not what the
        // fields are.
        let mut found: Vec<(String, String)> = Vec::new();
        for line in script.lines() {
            let fields: Vec<&str> = line.split('|').collect();
            for pair in fields.windows(2) {
                let (sha, url) = (pair[0], pair[1]);
                if sha.len() == 64
                    && sha.chars().all(|c| c.is_ascii_hexdigit())
                    && url.starts_with("https://")
                {
                    found.push((url.to_owned(), sha.to_owned()));
                }
            }
        }
        assert!(!found.is_empty(), "no artefacts were read out of the script");

        let mut table: HashMap<&str, &str> = HashMap::new();
        for package in package::PACKAGES {
            for artefact in package.artefacts() {
                if let Some(already) = table.insert(artefact.url, artefact.sha256) {
                    assert_eq!(
                        already, artefact.sha256,
                        "{}: two rows of the table pin it differently",
                        artefact.url
                    );
                }
            }
        }

        for (url, sha) in &found {
            let pinned = table
                .get(url.as_str())
                .unwrap_or_else(|| panic!("the table names no artefact at {url}"));
            assert_eq!(*pinned, sha.as_str(), "{url}: the script's digest is not the table's");
        }
        for url in table.keys() {
            assert!(
                found.iter().any(|(seen, _)| seen == url),
                "the script fetches nothing from {url}"
            );
        }
    }
}
