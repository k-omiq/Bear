#[path = "src/cloud_code_digest.rs"]
mod cloud_code_digest;

use sha2::{Digest, Sha256};
use std::path::Path;

/// Why the frozen cloud helper does not match this checkout, or `None` when it does.
///
/// `.github/scripts/build-cloud-provisioner.py` writes `<helper>.sha256` beside
/// the binary: one `sha256sum` line for every source file it froze, with paths
/// from the checkout root. Any line that no longer matches means the binary
/// runs older setup code than the checkout holds.
fn helper_stale_reason(root: &Path, stamp: &Path) -> Option<String> {
    println!("cargo:rerun-if-changed={}", stamp.display());
    let Ok(text) = std::fs::read_to_string(stamp) else {
        return Some(format!("{} is missing", stamp.display()));
    };
    let mut changed = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Some((digest, file)) = line.split_once("  ") else {
            return Some(format!("{} is malformed", stamp.display()));
        };
        let path = root.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let current = std::fs::read(&path).map(|bytes| format!("{:x}", Sha256::digest(bytes)));
        if current.ok().as_deref() != Some(digest) {
            changed.push(file.to_string());
        }
    }
    match changed.len() {
        0 => None,
        1 => Some(format!("{} changed since it was frozen", changed[0])),
        n => Some(format!(
            "{} and {} more files changed since it was frozen",
            changed[0],
            n - 1
        )),
    }
}

fn main() {
    // The cloud code the bundled helper deploys, so Settings can say when a
    // deployment runs other code (`cloud_code_digest.rs`).
    let deploy = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy");
    for watched in ["", "cloud", "cloud/common", "cloud/modal"] {
        println!("cargo:rerun-if-changed={}", deploy.join(watched).display());
    }
    for file in cloud_code_digest::shipped_files(&deploy).expect("list the cloud code in deploy/") {
        println!("cargo:rerun-if-changed={}", deploy.join(file).display());
    }
    let digest = cloud_code_digest::code_digest(&deploy).expect("hash the cloud code in deploy/");
    println!("cargo:rustc-env=MC_CLOUD_CODE_DIGEST={digest}");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    let suffix = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let helper = manifest
        .join("binaries")
        .join(format!("manga-cleaner-provisioner-{target}{suffix}"));
    // A helper frozen from older sources offers old models and old deploy
    // code. A development build then runs the live Python instead
    // (`crate::provision`); a release build refuses to ship it, below.
    let stamp = helper.with_file_name(format!("manga-cleaner-provisioner-{target}{suffix}.sha256"));
    let root = manifest.parent().expect("the checkout holds src-tauri");
    let stale = helper
        .is_file()
        .then(|| helper_stale_reason(root, &stamp))
        .flatten();
    let fresh = helper.is_file() && stale.is_none();
    println!("cargo:rustc-env=MC_FROZEN_HELPER_FRESH={}", u8::from(fresh));
    // A release app without this sidecar shows Cloud but cannot connect to
    // either provider. Fail the build instead of shipping that broken state.
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        let config: serde_json::Value = serde_json::from_slice(
            &std::fs::read(manifest.join("tauri.conf.json")).expect("read Tauri config"),
        )
        .expect("parse Tauri config");
        let bundled = config["bundle"]["externalBin"]
            .as_array()
            .is_some_and(|bins| {
                bins.iter()
                    .any(|bin| bin == "binaries/manga-cleaner-provisioner")
            });
        assert!(
            bundled,
            "release build is missing bundle.externalBin for the cloud helper; run .github/scripts/build-cloud-provisioner.py first"
        );
        assert!(
            helper.is_file(),
            "release build is missing {}. Freeze the cloud helper for this target first (npm run helpers)",
            helper.display()
        );
        if let Some(reason) = &stale {
            panic!(
                "the frozen cloud helper is out of date: {reason}. Rebuild it with npm run helpers"
            );
        }
        let uv_configured = config["bundle"]["externalBin"]
            .as_array()
            .is_some_and(|bins| bins.iter().any(|bin| bin == "binaries/manga-cleaner-uv"));
        assert!(
            uv_configured,
            "release build is missing bundle.externalBin for managed Python; run .github/scripts/stage-flux-python.py first"
        );
        let uv = manifest
            .join("binaries")
            .join(format!("manga-cleaner-uv-{target}{suffix}"));
        assert!(
            uv.is_file(),
            "release build is missing {}. Stage uv for this target first",
            uv.display()
        );
        println!("cargo:rerun-if-changed={}", helper.display());
        println!("cargo:rerun-if-changed={}", uv.display());
        println!("cargo:rerun-if-changed=tauri.conf.json");
    }
    if let Some(reason) = &stale {
        println!(
            "cargo:warning=frozen cloud helper is out of date ({reason}); this build runs python -m provisioner instead. npm run helpers rebuilds it"
        );
    }
    // `crate::provision` looks for the development sidecar under the name the
    // release script gives it, which ends in the target triple.
    println!("cargo:rustc-env=MC_TARGET_TRIPLE={target}");
    // The manifest that asks Windows for Common Controls v6. tauri-build embeds
    // it as a resource, and cargo links a resource into binaries only, so the
    // unit-test executable started without it and died before `main` with
    // STATUS_ENTRYPOINT_NOT_FOUND. The linker embeds the same manifest instead,
    // into everything this package links: the app, the tests, the examples.
    let windows = if target.contains("windows-msvc") {
        let app_manifest = manifest.join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", app_manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", app_manifest.display());
        tauri_build::WindowsAttributes::new_without_app_manifest()
    } else {
        tauri_build::WindowsAttributes::new()
    };
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(windows)
        .app_manifest(tauri_build::AppManifest::new().commands(&[
            "diagnostics",
            "about",
            "read_settings",
            "write_settings",
            "read_inference_config",
            "write_inference_config",
            "select_cloud_profile",
            "store_cloud_secret",
            "delete_cloud_secret",
            "get_cloud_secret_summary",
            "forget_cloud_secret_denials",
            "check_cloud_connection",
            "get_cloud_model_info",
            "check_cloud_release",
            "get_cloud_gpu_status",
            "stop_cloud_gpu",
            "list_remote_analysis_capabilities",
            "propose_remote_analysis",
            "confirm_remote_analysis",
            "cancel_remote_analysis",
            "get_remote_analysis_status",
            "propose_run_analysis",
            "confirm_run_analysis",
            "cancel_run_analysis",
            "prepare_cloud_clean",
            "confirm_cloud_clean",
            "start_cloud_clean",
            "cancel_cloud_clean",
            "prepare_cloud_denoise",
            "confirm_cloud_denoise",
            "start_cloud_denoise",
            "cancel_cloud_denoise",
            "cloud_denoise_presets",
            "prepare_cloud_consent",
            "confirm_cloud_consent",
            "submit_cloud_attempt",
            "get_cloud_attempt_status",
            "get_cloud_attempt_result",
            "resolve_qwen_review",
            "cancel_cloud_attempt",
            "reconcile_cloud_recovery",
            "run_cloud_provisioner",
            "provision_inspect",
            "provision_plan",
            "provision_apply",
            "provision_resume",
            "provision_cleanup",
            "provision_probe",
            "cancel_cloud_provisioner",
            "list_projects",
            "create_project",
            "create_chapter",
            "open_chapter",
            "load_pages",
            "history_load",
            "history_push",
            "history_move",
            "rename_project",
            "delete_project",
            "delete_chapter",
            "delete_mask",
            "restore_region",
            "set_layer_style",
            "keep_dependency_result",
            "set_detection_type",
            "apply_tool",
            "create_region",
            "preview_paint",
            "edit_detection_mask",
            "set_detection_padding",
            "rerun_mask",
            "clean_anyway",
            "sidecar_available",
            "list_sidecar_models",
            "pick_screen_color",
            "install_flux_helper",
            "export_chapter",
            "plan_export_chapter",
            "subscribe_events",
            "unsubscribe_events",
            "list_loaded_models",
            "unload_model",
            "list_accelerators",
            "list_workflow_capabilities",
            "import_full_rt",
            "remove_full_rt",
            "import_sam_ts",
            "install_sam_ts",
            "remove_sam_ts",
            "verify_sam_ts",
            "analyze_capabilities",
            "analyze_chapter_page",
            "cancel_capability_analysis",
            "load_component_correction",
            "prepare_component_write",
            "apply_component_write",
            "list_models",
            "download_model",
            "download_model_group",
            "cancel_download",
            "delete_model",
            "delete_model_group",
            "discard_partial",
            "verify_model",
            "verify_model_group",
            "download_runtime",
            "delete_runtime",
            "run_clean",
            "cancel_run",
            "resume_job",
            "denoise_chapter_local",
            "cancel_denoise_local",
            "benchmark_denoise_local",
            "denoise_presets",
            "replace_with_denoised",
            "denoise_history",
            "denoise_compare_image",
            "list_jobs",
            "confirm_quit",
            "hide_to_tray",
        ]));
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
