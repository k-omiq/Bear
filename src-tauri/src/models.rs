//! What is loaded, for the tab in the corner of the editor.
//!
//! Two commands over [`cleaner_core::registry`], and neither of them touches a
//! session. `list_loaded_models` reads the rows; `unload_model` sets a flag on
//! one, which the thread that owns the session reads at its next region
//! boundary and acts on there ([`crate::run::Pipeline::reap`]). Nothing here
//! can interrupt an inference, which is the property that makes a close button
//! on a model safe to offer at all.
//!
//! **What the user sees is therefore one poll behind the truth, in both
//! directions**, and the interface is written for that: a model can finish and
//! vanish between two polls, and a model asked to unload keeps its row until
//! the run reaches a safe point. The row says which of those it is -
//! `unloading` - so the tab can show the request rather than pretending it
//! already happened.
//!
//! No English crosses the seam: a row
//! carries the i18n key for what it is and the key for where it is running, and
//! the interface names both.
//!
//! ## And a third: where models *would* run
//!
//! `list_accelerators` is the same subject one step earlier. The two above
//! answer "what is loaded"; this one answers "what can this machine run models
//! on, which of those will it use, and why" - the question the accelerator
//! setting is made of. It lives here rather than beside `about` because it is
//! about models and hardware rather than about the licence, and because it
//! shares this module's rule that a row carries keys and never English.
//!
//! It is also where the reporting side is closed: `accel::Selection::declined`
//! has carried four refusal reasons and reached no interface. Every one of
//! them is on this call's per-model rows now, with the two byte figures the
//! memory refusal carries.

use serde::Serialize;

use cleaner_core::accel::{self, Accelerator};
use cleaner_core::registry;

/// The detection models' names, as every screen, notice and error shows them.
/// The ids beside them (`ctd`, `rtSmall`, `rtFull`, `samTs`) are persisted and
/// never change; these names are the only thing a rename touches. Full and
/// Small are the two published profiles of one upstream model,
/// `ogkalu/comic-text-and-bubble-detector`, and a run takes at most one.
pub(crate) const CTD_NAME: &str = "Comic Text Detector (CTD)";
pub(crate) const RT_SMALL_NAME: &str = "Ogkalu comic text & bubble detector (Small)";
pub(crate) const RT_FULL_NAME: &str = "Ogkalu comic text & bubble detector (Full)";
pub(crate) const SAM_TS_NAME: &str = "SAM-TS-L lettering mask";
/// The text reader (`hayai`), named as its download is.
pub(crate) const HAYAI_NAME: &str = "Hayai OCR v2.5 Nova";

/// One loaded thing.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoadedModel {
    /// The registry's id, which is what `unload_model` takes. Not stable
    /// across loads - a model unloaded and opened again is a new row, because
    /// it is a new session.
    pub id: u64,
    /// e.g. `models.kind.inpainter`.
    pub kind_key: &'static str,
    pub model_name: Option<String>,
    /// Roughly how much memory it is holding. See `basis`.
    pub bytes: u64,
    /// How `bytes` was arrived at: `measured`, `weights` or `reported`. The
    /// interface does not draw this today - the tab says "about 510 MB"
    /// whichever it is - and it is on the wire because a number whose
    /// provenance is not carried is a number nobody can correct later.
    pub basis: &'static str,
    /// e.g. `accel.coreml`.
    pub device_key: &'static str,
    /// Whether it is on the graphics device.
    pub gpu: bool,
    /// How long since anything used it, in milliseconds.
    pub idle_ms: u64,
    /// Whether an unload has been asked for and not yet reached a safe point.
    pub unloading: bool,
}

fn basis_key(basis: registry::Basis) -> &'static str {
    match basis {
        registry::Basis::Measured => "measured",
        registry::Basis::Weights => "weights",
        registry::Basis::Reported => "reported",
    }
}

fn row(loaded: registry::Loaded) -> LoadedModel {
    LoadedModel {
        id: loaded.id,
        kind_key: loaded.kind.label_key(),
        model_name: loaded.model_name,
        bytes: loaded.bytes,
        basis: basis_key(loaded.basis),
        device_key: loaded.device.label_key,
        gpu: loaded.device.gpu,
        idle_ms: loaded.idle_ms,
        unloading: loaded.unload_requested,
    }
}

/// Everything resident in this process, and the sidecar child if one is up.
///
/// Synchronous: it is a walk over a handful of rows under one mutex, and
/// putting it on the blocking pool would cost more than it takes.
#[tauri::command]
pub fn list_loaded_models() -> Vec<LoadedModel> {
    // **The poll is also the clock.** A session nobody is using is parked in
    // [`cleaner_core::residency`], and something has to notice when its grace
    // has run out. The tab asks every two seconds while the editor is open,
    // which is both the cheapest place to look and the one where the answer is
    // about to be drawn - so a row that has just expired disappears in the same
    // poll that would otherwise have shown it. The residency sweeper covers the
    // hours when nothing is polling.
    cleaner_core::residency::sweep();
    registry::loaded().into_iter().map(row).collect()
}

/// Ask for one back.
///
/// `false` means there is no such row - the model had already gone by the time
/// the click arrived, which the interface reports as nothing rather than as a
/// failure. `true` means the request is recorded, **not** that the memory is
/// free: the owning thread drops the session at its next region boundary.
///
/// **A model nothing is using goes on this call**, which is new: a session
/// parked in [`cleaner_core::residency`] has no owner to reach a safe point,
/// and the safe point for one nobody holds is now. So the close button is
/// immediate for every idle row and a request for every busy one - the
/// distinction the row's `unloading` field already carried.
#[tauri::command]
pub fn unload_model(id: u64) -> bool {
    let asked = registry::request_unload(id);
    cleaner_core::residency::sweep();
    asked
}

/// The `accel.` prefix is how the catalogue namespaces a provider; the setting
/// stores what is left, so `accel.webgpu` is written down as `webgpu`.
fn accelerator_id(accelerator: Accelerator) -> &'static str {
    accelerator.label_key().strip_prefix("accel.").unwrap_or(accelerator.label_key())
}

/// One provider, as the settings panel draws it.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcceleratorRow {
    /// What the setting stores: `cpu`, `directml`, `webgpu`, …
    pub id: &'static str,
    /// e.g. `accel.directml`.
    pub label_key: &'static str,
    pub available: bool,
    /// Why not, when it is not - and the two are different remedies.
    /// `accel.declined.unavailable` is a different **download**;
    /// `accel.declined.missingRuntime` is an **install** the user has to do.
    pub reason_key: Option<&'static str>,
    /// Whether any model here has a timing on this provider. The difference
    /// between "WebGPU (measured)" and "DirectML (unmeasured)", and the reason
    /// this field exists rather than a badge invented in the interface.
    pub measured: bool,
    /// Whether the setting in force right now puts any model on it.
    pub active: bool,
    /// Whether this is the stored choice.
    pub selected: bool,
}

/// Where one model will run, under the setting in force.
///
/// **This is the half that was missing.** `accel::Selection::declined` has
/// carried four reasons and two figures and reached no interface, so a forced
/// provider refused for want of memory was refused silently. It is on the
/// wire here, per model,
/// with the two byte counts beside the key - the key is translatable and the
/// numbers are not, which is why they travel separately.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPlacement {
    /// Stable setting id, independent of translated model names.
    pub id: &'static str,
    pub model_name: &'static str,
    pub preference: &'static str,
    pub supported_ids: Vec<&'static str>,
    pub backend_status: Vec<ModelBackendStatus>,
    /// e.g. `models.kind.inpainter` - the same key the loaded-models tab uses.
    pub model_key: &'static str,
    pub accelerator_id: &'static str,
    /// e.g. `accel.webgpu`.
    pub label_key: &'static str,
    /// `accel.chosen.unmeasured` when the choice rests on mechanism rather than
    /// on a timing.
    pub note_key: Option<&'static str>,
    /// Set when the user's provider was not used, or was used and is a bad
    /// idea. The provider it is about is `declinedId`.
    pub declined_key: Option<&'static str>,
    pub declined_id: Option<&'static str>,
    /// What the declined provider was measured to need, and what there was room
    /// for. Both `None` unless the refusal was `accel.declined.memory`.
    pub needed_bytes: Option<u64>,
    pub room_bytes: Option<u64>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelBackendStatus {
    pub id: &'static str,
    pub supported: bool,
    pub installed: bool,
    pub available: bool,
    pub verified: bool,
    pub reason_key: Option<&'static str>,
}

/// Everything the accelerator setting needs to draw itself.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Accelerators {
    /// The stored setting, normalised: `auto`, `cpu`, or a provider id. Never
    /// absent - an unwritten or unreadable value reads as `auto`, which is what
    /// the backend will actually do.
    pub preference: &'static str,
    pub providers: Vec<AcceleratorRow>,
    pub models: Vec<ModelPlacement>,
}

/// What this machine can run models on, what it will use, and why.
///
/// Two questions in one call because they have one answer: a provider row that
/// says "available" without saying whether anything is *on* it would leave the
/// panel to re-derive the routing, and the routing is per model and lives in
/// `cleaner_core::accel`. The interface renders; it does not decide.
///
/// **The runtime is loaded here if it is not already**, for the same reason
/// `run` and `region` do it: the download happens after install, so "which
/// providers exist" is a question whose answer changes over a session's life.
/// A failure is not an error - it is a machine with the CPU and nothing else,
/// which is exactly what the rows then say. `diagnostics.runtime.*` is where
/// the remedy for that lives.
#[tauri::command]
pub fn list_accelerators(app: tauri::AppHandle) -> Accelerators {
    let app_data = {
        use tauri::Manager;
        app.path().app_data_dir().ok()
    };
    let runtime_loaded = cleaner_core::runtime::find(app_data.as_deref())
        .and_then(|path| cleaner_core::runtime::load(&path)).is_ok();

    let settings = crate::settings::read(&app).unwrap_or(serde_json::Value::Null);
    let verified_runtime = app_data.as_deref()
        .is_some_and(crate::model_workflows::qualified_webgpu_write_environment);
    let mut view = accelerators_configured(
        crate::run::preference_from(&settings),
        Some(&settings),
        &if runtime_loaded { accel::available() } else { Vec::new() },
        cleaner_core::memory::room(),
        cleaner_core::runtime::package::Platform::host(),
    );

    // `accelerators` is pure and can only say "not in the list"; the machine
    // knows the finer answer, and this is the one place with the machine in
    // reach. Same upgrade `accel::open_session` makes to a `Declined`, for the
    // same reason: a build that carries CUDA on a machine with no CUDA runtime
    // needs an install, not a different download. The rows are `KNOWN`'s order
    // by construction, so they zip.
    for (row, accelerator) in view.providers.iter_mut().zip(accel::KNOWN) {
        if !runtime_loaded {
            row.available = false;
            row.reason_key = Some("accel.declined.unavailable");
            continue;
        }
        if runtime_loaded && !row.available {
            if let Err(reason) = accelerator.availability() {
                row.reason_key = Some(reason.reason_key());
            }
        }
    }
    for model in &mut view.models {
        for status in &mut model.backend_status {
            let accelerator = accel::KNOWN.into_iter().find(|a| accelerator_id(*a) == status.id).unwrap();
            status.installed = runtime_loaded && (accelerator == Accelerator::Cpu || accelerator.is_installed());
            status.verified &= verified_runtime;
            status.available = status.supported && runtime_loaded && accelerator.is_available()
                && (model.id != "samTs" || accelerator != Accelerator::WebGpu
                    || cleaner_core::sam_ts::built_in_webgpu_available());
            if status.supported && !status.available {
                status.reason_key = if runtime_loaded {
                    accelerator.availability().err().map(accel::Unavailable::reason_key)
                        .or(Some("accel.declined.noDevice"))
                } else { Some("accel.declined.unavailable") };
            }
        }
    }
    view
}

/// The same, with the machine passed in. Split out for
/// `accel::choose_on`'s reason: every rule above is testable without a GPU.
///
/// **Pure, and therefore coarse about one thing.** A provider absent from
/// `available` is reported as `accel.declined.unavailable`, because a list is
/// all this function has; the caller above upgrades that to
/// `accel.declined.missingRuntime` or `accel.declined.noDevice` where the
/// machine says the provider is in the build and what it needs is not.
///
/// `host` is the platform the panel is drawn on: the placements and the
/// `measured` flag both depend on whether it is the one the table was timed
/// on ([`accel::MEASURED_ON`]).
#[cfg(test)]
pub fn accelerators(
    preference: accel::Preference,
    available: &[Accelerator],
    room: Option<u64>,
    host: Option<cleaner_core::runtime::package::Platform>,
) -> Accelerators {
    accelerators_configured(preference, None, available, room, host)
}

pub(crate) fn supports_model(id: &str, accelerator: Accelerator, host: Option<cleaner_core::runtime::package::Platform>) -> bool {
    use cleaner_core::runtime::package::Os;
    if accelerator == Accelerator::Cpu { return true; }
    match id {
        "ctd" => match accelerator {
            Accelerator::DirectMl => host.is_some_and(|h| h.os == Os::Windows),
            Accelerator::Cuda => host.is_some_and(|h| matches!(h.os, Os::Windows | Os::Linux)),
            Accelerator::WebGpu => true,
            _ => false,
        },
        "rtFull" => match accelerator {
            Accelerator::DirectMl => host.is_some_and(|h| h.os == Os::Windows),
            Accelerator::Cuda => host.is_some_and(|h| matches!(h.os, Os::Windows | Os::Linux)),
            _ => false,
        },
        "rtSmall" => (accelerator == Accelerator::DirectMl && host.is_some_and(|h| h.os == Os::Windows))
            || (accelerator == Accelerator::Cuda && host.is_some_and(|h| matches!(h.os, Os::Windows | Os::Linux))),
        "samTs" => accelerator == Accelerator::WebGpu && host.is_some_and(|h| h.os == Os::MacOs),
        // CUDA only: it ran the reader on a cloud L4. WebGPU fails its vision
        // graph at the first read and CoreML its decoder, and DirectML is
        // untested (`accel::HAYAI`).
        "hayai" => accelerator == Accelerator::Cuda && host.is_some_and(|h| matches!(h.os, Os::Windows | Os::Linux)),
        // Every GPU provider the platform has but CoreML: the forced session
        // keeps the CPU fallback for what the provider cannot hold
        // (`accel::LAMA.partitioned_on`). CoreML is left out: it has no `DFT`,
        // measured slower than the CPU, and peaked at 8.4 GB (`accel::LAMA`).
        "inpainter" => match accelerator {
            Accelerator::WebGpu => true,
            Accelerator::DirectMl => host.is_some_and(|h| h.os == Os::Windows),
            Accelerator::Cuda => host.is_some_and(|h| matches!(h.os, Os::Windows | Os::Linux)),
            _ => false,
        },
        _ => false,
    }
}

fn verified_model(id: &str, accelerator: Accelerator, measured_host: bool) -> bool {
    if accelerator == Accelerator::Cpu { return measured_host; }
    measured_host && matches!(
        (id, accelerator),
        ("ctd", Accelerator::WebGpu)
            | ("samTs", Accelerator::WebGpu)
    )
}

fn accelerators_configured(
    preference: accel::Preference,
    settings: Option<&serde_json::Value>,
    available: &[Accelerator],
    room: Option<u64>,
    host: Option<cleaner_core::runtime::package::Platform>,
) -> Accelerators {
    let measured_here = host.map(|platform| platform.os) == Some(accel::MEASURED_ON);
    let verified_here = measured_here && cfg!(target_os = "macos")
        && cleaner_core::sam_ts::measured_m5_host();
    let specs: [(&str, &str, &accel::ModelProfile); 6] = [
        ("ctd", CTD_NAME, &accel::DETECTOR),
        ("rtSmall", RT_SMALL_NAME, &accel::BALLOON),
        ("rtFull", RT_FULL_NAME, &accel::FULL_RT),
        ("samTs", SAM_TS_NAME, &accel::FULL_RT),
        ("inpainter", "LaMa Manga", &accel::LAMA),
        ("hayai", HAYAI_NAME, &accel::HAYAI),
    ];
    let placements: Vec<(Accelerator, ModelPlacement)> = specs
        .iter()
        .map(|(id, model_name, profile)| {
            let requested = settings.and_then(|s| crate::run::model_preference_from(s, id).ok()).unwrap_or(preference);
            let mut chosen = accel::choose_on(profile, requested, available, room, host);
            if !supports_model(id, chosen.accelerator, host) || matches!(requested, accel::Preference::Force(wanted) if !supports_model(id, wanted, host)) {
                let wanted = match requested { accel::Preference::Force(wanted) => wanted, _ => chosen.accelerator };
                chosen.accelerator = Accelerator::Cpu;
                chosen.declined = Some(accel::Declined {
                    wanted,
                    reason_key: "accel.declined.unavailable",
                    needed_bytes: None,
                    room_bytes: None,
                });
                chosen.note = None;
            }
            let declined = chosen.declined;
            let backend_status = accel::KNOWN.into_iter().map(|accelerator| {
                let supported = supports_model(id, accelerator, host);
                let installed = accelerator == Accelerator::Cpu || available.contains(&accelerator);
                let available = supported && installed;
                ModelBackendStatus {
                    id: accelerator_id(accelerator), supported, installed, available,
                    verified: verified_model(id, accelerator, verified_here),
                    reason_key: (supported && !available).then_some("accel.declined.unavailable"),
                }
            }).collect::<Vec<_>>();
            (
                chosen.accelerator,
                ModelPlacement {
                    id,
                    model_name,
                    preference: preference_id(requested),
                    supported_ids: backend_status.iter().filter(|s| s.supported).map(|s| s.id).collect(),
                    backend_status,
                    model_key: profile.label_key,
                    accelerator_id: accelerator_id(chosen.accelerator),
                    label_key: chosen.accelerator.label_key(),
                    note_key: chosen.note,
                    declined_key: declined.map(|d| d.reason_key),
                    declined_id: declined.map(|d| accelerator_id(d.wanted)),
                    needed_bytes: declined.and_then(|d| d.needed_bytes),
                    room_bytes: declined.and_then(|d| d.room_bytes),
                },
            )
        })
        .collect();

    let providers = accel::KNOWN
        .into_iter()
        .map(|accelerator| {
            // The CPU is always there, which is `choose_within`'s own rule.
            let present = accelerator == Accelerator::Cpu || available.contains(&accelerator);
            AcceleratorRow {
                id: accelerator_id(accelerator),
                label_key: accelerator.label_key(),
                available: present,
                reason_key: (!present).then_some(accel::Unavailable::NotInRuntime.reason_key()),
                // A measurement is a measurement on the machine it was made
                // on; the same provider elsewhere is a guess, and the row
                // says so.
                measured: measured_here
                    && accel::PROFILES
                        .iter()
                        .any(|profile| profile.measured_better.contains(&accelerator)),
                active: placements.iter().any(|(on, _)| *on == accelerator),
                selected: preference_id(preference) == accelerator_id(accelerator),
            }
        })
        .collect();

    Accelerators {
        preference: preference_id(preference),
        providers,
        models: placements.into_iter().map(|(_, placement)| placement).collect(),
    }
}

/// The stored spelling of a preference. The inverse of
/// [`crate::run::accelerator_preference`], and it round-trips: what this writes
/// is what that reads.
fn preference_id(preference: accel::Preference) -> &'static str {
    match preference {
        accel::Preference::Automatic => "auto",
        accel::Preference::CpuOnly => "cpu",
        accel::Preference::Force(accelerator) => accelerator_id(accelerator),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::runtime::package::{Arch, Os, Platform};
    use std::path::Path;

    const MAC: Platform = Platform { os: Os::MacOs, arch: Arch::Aarch64 };
    const WINDOWS: Platform = Platform { os: Os::Windows, arch: Arch::X86_64 };

    /// The row the interface receives, field for field. The keys are what the
    /// catalogue has to carry, and a rename here is a blank line in the tab.
    #[test]
    fn a_row_carries_keys_and_never_english() {
        let lease = registry::register(
            registry::Kind::Inpainter,
            registry::Footprint::measured(510 * 1024 * 1024),
            registry::Device::accelerator(cleaner_core::accel::Accelerator::CoreMl),
        );
        let rows = list_loaded_models();
        let row = rows.iter().find(|row| row.id == lease.id()).expect("no row");
        assert_eq!(row.kind_key, "models.kind.inpainter");
        assert_eq!(row.device_key, "accel.coreml");
        assert_eq!(row.basis, "measured");
        assert!(row.gpu);
        assert!(!row.unloading);
        assert_eq!(row.bytes, 510 * 1024 * 1024);
    }

    /// An unload is recorded on the row and the row stays: the session is still
    /// loaded until its owner reaches a safe point, and a tab that removed the
    /// row on the click would be claiming memory back that is still held.
    #[test]
    fn unloading_marks_the_row_rather_than_removing_it() {
        let lease = registry::register(
            registry::Kind::TextDetector,
            registry::Footprint::weights(Path::new("/nonexistent")),
            registry::Device::accelerator(cleaner_core::accel::Accelerator::Cpu),
        );
        assert!(unload_model(lease.id()));
        let rows = list_loaded_models();
        let row = rows.iter().find(|row| row.id == lease.id()).expect("the row went early");
        assert!(row.unloading);
        assert!(lease.spent(), "the owner was not told to give it back");
    }

    /// **A Windows machine with the DirectML download**, as the settings panel
    /// would draw it: every model on the GPU, every one of them flagged
    /// unmeasured, DirectML shown as active but not measured - and WebGPU,
    /// which the plugin beside the runtime provides, shown as neither
    /// measured nor the inpainter's choice, because on this platform DirectML
    /// is the candidate written for it.
    #[test]
    fn the_panel_says_where_each_model_runs_and_whether_anyone_timed_it() {
        let windows = [Accelerator::DirectMl, Accelerator::WebGpu];
        let view = accelerators(accel::Preference::Automatic, &windows, None, Some(WINDOWS));

        assert_eq!(view.preference, "auto");
        let dml = view.providers.iter().find(|row| row.id == "directml").expect("a row");
        assert!(dml.available);
        assert_eq!(dml.reason_key, None);
        assert!(dml.active);
        assert!(!dml.measured, "nothing here has been timed on Windows");
        assert!(!dml.selected, "`auto` selects no provider");

        // The CPU is always a row, always available, and here it is the one the
        // two small models are on.
        let cpu = view.providers.iter().find(|row| row.id == "cpu").expect("a row");
        assert!(cpu.available);
        assert!(cpu.active);

        // A provider this build does not have says which absence it is.
        let cuda = view.providers.iter().find(|row| row.id == "cuda").expect("a row");
        assert!(!cuda.available);
        assert!(cuda.reason_key.is_some());

        let inpainter = view
            .models
            .iter()
            .find(|row| row.model_key == "models.kind.inpainter")
            .expect("the inpainter");
        assert_eq!(inpainter.accelerator_id, "directml");
        assert_eq!(inpainter.label_key, "accel.directml");
        assert_eq!(inpainter.note_key, Some("accel.chosen.unmeasured"));
        assert_eq!(inpainter.declined_key, None);

        let webgpu = view.providers.iter().find(|row| row.id == "webgpu").expect("a row");
        assert!(webgpu.available);
        assert!(!webgpu.measured, "WebGPU was measured on Metal, and this is Direct3D");
        assert!(!webgpu.active, "DirectML is the candidate written for this platform");

        // The same list read on the machine the table was measured on says
        // the opposite about WebGPU, which is the whole reason `host` is an
        // argument.
        let view = accelerators(accel::Preference::Automatic, &windows, None, Some(MAC));
        let webgpu = view.providers.iter().find(|row| row.id == "webgpu").expect("a row");
        assert!(webgpu.measured);
        assert!(webgpu.active);
    }

    /// **A Linux machine with the stock download**, which is the plugin and
    /// nothing else: the two GPU models on WebGPU, both guesses, and the
    /// provider row for it neither measured nor absent.
    #[test]
    fn the_panel_draws_a_linux_machine_with_the_plugin_on_the_gpu() {
        let linux = [Accelerator::WebGpu];
        let host = Some(Platform { os: Os::Linux, arch: Arch::X86_64 });
        let view = accelerators(accel::Preference::Automatic, &linux, None, host);
        let webgpu = view.providers.iter().find(|row| row.id == "webgpu").expect("a row");
        assert!(webgpu.available && webgpu.active && !webgpu.measured);
        for key in ["models.kind.inpainter", "models.kind.textDetector"] {
            let row = view.models.iter().find(|row| row.model_key == key).expect(key);
            assert_eq!(row.accelerator_id, "webgpu", "{key}");
            assert_eq!(row.note_key, Some("accel.chosen.unmeasured"), "{key}");
        }
    }

    /// **The other half.** A forced provider refused for want of memory now
    /// reaches an interface, with the key and both figures on it.
    #[test]
    fn a_refusal_reaches_the_panel_with_its_two_figures() {
        let mac = [Accelerator::CoreMl, Accelerator::WebGpu];
        // A small memory budget, below WebGPU LaMa's measured 740 MiB peak.
        let room = 700 * cleaner_core::memory::MIB;
        let view =
            accelerators(accel::Preference::Force(Accelerator::WebGpu), &mac, Some(room), Some(MAC));

        assert_eq!(view.preference, "webgpu");
        assert!(view.providers.iter().find(|row| row.id == "webgpu").unwrap().selected);

        let inpainter = view
            .models
            .iter()
            .find(|row| row.model_key == "models.kind.inpainter")
            .expect("the inpainter");
        assert_eq!(inpainter.accelerator_id, "cpu");
        assert_eq!(inpainter.declined_key, Some("accel.declined.memory"));
        assert_eq!(inpainter.declined_id, Some("webgpu"));
        assert!(inpainter.needed_bytes.is_some());
        assert_eq!(inpainter.room_bytes, Some(room));
    }

    /// The setting round-trips: what the panel is shown is what
    /// [`crate::run::accelerator_preference`] reads back.
    #[test]
    fn every_provider_id_the_panel_offers_is_one_the_run_can_read_back() {
        let view = accelerators(accel::Preference::Automatic, &[], None, Some(MAC));
        assert_eq!(view.providers.len(), accel::KNOWN.len());
        for row in &view.providers {
            let read_back = crate::run::accelerator_preference(row.id);
            let expected = if row.id == "cpu" {
                accel::Preference::CpuOnly
            } else {
                accel::Preference::Force(
                    accel::KNOWN.into_iter().find(|a| super::accelerator_id(*a) == row.id).unwrap(),
                )
            };
            assert_eq!(read_back, expected, "{}", row.id);
        }
        assert_eq!(crate::run::accelerator_preference("auto"), accel::Preference::Automatic);
        // A value from a newer build, or typed by hand, is `Automatic` rather
        // than a failure to clean a page.
        assert_eq!(crate::run::accelerator_preference("nonsense"), accel::Preference::Automatic);
    }

    /// `CpuOnly` is absolute here too: the panel shows every model on the CPU
    /// and no provider active but the CPU, whatever the machine has.
    #[test]
    fn cpu_only_puts_every_model_on_the_cpu_in_the_panel() {
        let mac = [Accelerator::CoreMl, Accelerator::WebGpu];
        let view = accelerators(accel::Preference::CpuOnly, &mac, None, Some(MAC));
        assert_eq!(view.preference, "cpu");
        for row in &view.models {
            assert_eq!(row.accelerator_id, "cpu", "{}", row.model_key);
            assert_eq!(row.declined_key, None, "{}", row.model_key);
        }
        for row in view.providers.iter().filter(|row| row.id != "cpu") {
            assert!(!row.active, "{}", row.id);
        }
    }

    #[test]
    fn capability_rows_do_not_advertise_unsupported_backend_for_a_model() {
        let mac = [Accelerator::CoreMl, Accelerator::WebGpu];
        let settings = serde_json::json!({
            "accelerator": "auto",
            "modelAccelerators": {"samTs": "webgpu", "rtSmall": "coreml"}
        });
        let view = accelerators_configured(accel::Preference::Automatic, Some(&settings), &mac, None, Some(MAC));
        let sam = view.models.iter().find(|row| row.id == "samTs").unwrap();
        assert_eq!(sam.preference, "webgpu");
        assert_eq!(sam.accelerator_id, "webgpu");
        assert_eq!(sam.model_name, "SAM-TS-L lettering mask");
        assert!(sam.supported_ids.contains(&"webgpu"));
        assert!(!sam.supported_ids.contains(&"coreml"));
        let small = view.models.iter().find(|row| row.id == "rtSmall").unwrap();
        assert_eq!(small.preference, "coreml");
        assert_eq!(small.accelerator_id, "cpu");
        assert_eq!(small.declined_id, Some("coreml"));
        assert!(!small.backend_status.iter().find(|row| row.id == "coreml").unwrap().supported);
    }

    /// LaMa is offered every GPU provider the platform has but CoreML, because
    /// a forced session keeps the CPU fallback on each. The offer and that exemption
    /// must agree, or Settings offers a backend the model refuses to load on.
    #[test]
    fn the_inpainter_is_offered_every_gpu_and_the_forced_session_may_partition() {
        const LINUX: Platform = Platform { os: Os::Linux, arch: Arch::X86_64 };
        for (host, offered) in [
            (MAC, &[Accelerator::Cpu, Accelerator::WebGpu][..]),
            (WINDOWS, &[Accelerator::Cpu, Accelerator::DirectMl, Accelerator::Cuda, Accelerator::WebGpu][..]),
            (LINUX, &[Accelerator::Cpu, Accelerator::Cuda, Accelerator::WebGpu][..]),
        ] {
            for accelerator in accel::KNOWN {
                assert_eq!(supports_model("inpainter", accelerator, Some(host)), offered.contains(&accelerator),
                    "{accelerator:?} on {:?}", host.os);
                if accelerator != Accelerator::Cpu && offered.contains(&accelerator) {
                    assert!(accel::LAMA.partitioned_on.contains(&accelerator), "{accelerator:?}");
                }
            }
        }

        let mac = [Accelerator::CoreMl, Accelerator::WebGpu];
        let settings = serde_json::json!({
            "accelerator": "auto",
            "modelAccelerators": {"inpainter": "webgpu"}
        });
        let view = accelerators_configured(accel::Preference::Automatic, Some(&settings), &mac, None, Some(MAC));
        let lama = view.models.iter().find(|row| row.id == "inpainter").unwrap();
        assert_eq!(lama.preference, "webgpu");
        assert_eq!(lama.accelerator_id, "webgpu");
        assert_eq!(lama.declined_id, None);
        assert!(lama.supported_ids.contains(&"webgpu"));
        assert!(accel::LAMA.partitioned_on.contains(&Accelerator::WebGpu));
    }

    #[test]
    fn webgpu_verification_requires_strict_model_assignment_evidence() {
        assert!(verified_model("ctd", Accelerator::WebGpu, true));
        assert!(verified_model("samTs", Accelerator::WebGpu, true));
        // Earlier LaMa timings did not establish full graph assignment under
        // the new strict forced-provider contract.
        assert!(!verified_model("inpainter", Accelerator::WebGpu, true));
    }

    /// A click that arrives after the model has gone is not a failure.
    #[test]
    fn unloading_something_that_has_gone_is_not_an_error() {
        let id = {
            let lease = registry::register(
                registry::Kind::ScriptGate,
                registry::Footprint::measured(1),
                registry::Device::accelerator(cleaner_core::accel::Accelerator::Cpu),
            );
            lease.id()
        };
        assert!(!unload_model(id));
        assert!(!list_loaded_models().iter().any(|row| row.id == id));
    }
}
