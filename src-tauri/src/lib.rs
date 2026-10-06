//! The Tauri adapter.
//!
//! One process: the webview talks to
//! this over commands and a channel, and everything it asks for is answered by
//! `cleaner-core`. Nothing here holds pixels - pixels go to the browser over the
//! `tile://` protocol, and compositing and export happen in Rust, which is what
//! makes color fidelity enforceable rather than aspirational.

mod about;
mod cloud_code_digest;
mod denoise_history;
mod diagnostics;
mod events;
mod exporting;
mod flux_install;
mod history;
pub mod inference;
mod jobs;
mod library;
#[cfg(target_os = "macos")]
mod macos_keychain;
mod models;
mod model_workflows;
mod page_denoise;
pub mod provision;
mod region;
mod screen_color;
pub mod run;
fn app_context() -> tauri::Context<tauri::Wry> { tauri::generate_context!() }
mod settings;
mod smoke;
mod tile;
mod underlay;
mod weights;
mod webview_lock;
#[cfg(debug_assertions)]
pub mod live_demo;
#[cfg(debug_assertions)]
pub mod cloud_attempt_probe;
#[cfg(debug_assertions)]
pub mod release_probe;
#[cfg(debug_assertions)]
pub mod release_analysis_probe;
#[cfg(debug_assertions)]
pub mod release_chunk_fixture;
#[cfg(debug_assertions)]
pub mod release_recipe_probe;
#[cfg(debug_assertions)]
pub mod mask_stages;

/// The tray icon's id, so close-to-tray can check the icon exists.
const TRAY_ID: &str = "main";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::Manager;
    let builder = tauri::Builder::default();
    // First, so a second launch while the window is hidden in the tray shows
    // this one instead of starting another copy. A launch check is its own
    // copy: handing it to a running one would check nothing.
    let builder = if smoke::requested() {
        builder
    } else {
        builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
    };
    builder
        .on_page_load(smoke::on_page_load)
        .setup(|app| {
            if smoke::requested() {
                smoke::begin(app.handle());
            }
            webview_lock::install(app.handle());
            // Regions whose cloud work needs a look (a committed result gone
            // missing, one never applied), read from the attempt journal at
            // every start whether or not the cloud is allowed. Page reads wait
            // for it, so the first chapter opened already shows the flags.
            library::begin_cloud_attention_read();
            let handle = app.handle().clone();
            std::thread::spawn(move || inference::commands::refresh_cloud_attention(&handle));
            use tauri::{
                menu::{Menu, MenuItem},
                tray::TrayIconBuilder,
            };
            let show = MenuItem::with_id(app, "show", "Show Manga Cleaner", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Manga Cleaner", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let tray = TrayIconBuilder::with_id(TRAY_ID)
                .menu(&menu)
                .tooltip("Manga Cleaner")
                // A menu-bar icon opens its menu on click on macOS. Elsewhere a
                // left click brings the window back and the menu is on the
                // right button. Linux tray hosts do not report clicks, so there
                // the menu is the only route, which it always offers.
                .show_menu_on_left_click(cfg!(target_os = "macos"))
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if !cfg!(target_os = "macos") {
                            show_main(tray.app_handle());
                        }
                    }
                })
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main(app),
                    // Held by the quit guard at `ExitRequested` while jobs run.
                    "quit" => app.exit(0),
                    _ => {}
                });
            // The macOS menu bar wants a template image, black plus alpha,
            // which the system tints to match a light or dark menu bar; the
            // full-colour app icon would not follow it. The asset and its
            // generator live in `assets/tray/`. Other platforms keep the app
            // icon.
            #[cfg(target_os = "macos")]
            let tray = tray
                .icon(tauri::include_image!("assets/tray/tray-template@2x.png"))
                .icon_as_template(true);
            #[cfg(not(target_os = "macos"))]
            let tray = match app.default_window_icon() {
                Some(icon) => tray.icon(icon.clone()),
                None => tray,
            };
            // Not fatal: a Linux desktop without an AppIndicator host has no
            // tray, and the editor works without one. Close-to-tray checks
            // that the icon exists before it hides the window.
            if let Err(error) = tray.build(app) {
                eprintln!("tray icon unavailable: {error}");
            }
            #[cfg(target_os = "macos")]
            if let Err(error) = guard_menu_quit(app.handle()) {
                eprintln!("app menu quit not guarded: {error}");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Hiding the only window with no tray icon to bring it back
                // would leave the app running with no way to reach it.
                let keep_running = window.app_handle().tray_by_id(TRAY_ID).is_some()
                    && settings::read(window.app_handle())
                        .ok()
                        .and_then(|value| value.get("closeToTray").and_then(|v| v.as_bool()))
                        .unwrap_or(false);
                if keep_running {
                    api.prevent_close();
                    let _ = window.hide();
                } else if window.label() == "main" && hold_quit(window.app_handle()) {
                    // Jobs are running: the window stays until the answer.
                    api.prevent_close();
                }
            }
        })
        // The one capability the window has beyond `core:default`: a folder
        // chooser. Import cannot be real without it - a project and a chapter
        // each need a source folder, and until this plugin was registered the
        // only way to give one was to type an absolute path from memory. The
        // permission granted in `capabilities/default.json` is `dialog:allow-open`
        // and nothing else: no save dialog, no message boxes, and no filesystem
        // plugin, so the chooser hands back a path and every read of it still
        // goes through this crate's own commands.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Opens the project page from onboarding in the user's browser. The
        // capability allows that one URL and nothing else.
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            diagnostics::diagnostics,
            about::about,
            settings::read_settings,
            settings::write_settings,
            inference::commands::read_inference_config,
            inference::analysis::list_remote_analysis_capabilities,
            inference::analysis::propose_remote_analysis,
            inference::analysis::confirm_remote_analysis,
            inference::analysis::cancel_remote_analysis,
            inference::analysis::get_remote_analysis_status,
            inference::run_analysis::propose_run_analysis,
            inference::run_analysis::confirm_run_analysis,
            inference::run_analysis::cancel_run_analysis,
            inference::cloud_clean::prepare_cloud_clean,
            inference::cloud_clean::confirm_cloud_clean,
            inference::cloud_clean::start_cloud_clean,
            inference::cloud_clean::cancel_cloud_clean,
            inference::cloud_denoise::prepare_cloud_denoise,
            inference::cloud_denoise::confirm_cloud_denoise,
            inference::cloud_denoise::start_cloud_denoise,
            inference::cloud_denoise::cancel_cloud_denoise,
            inference::cloud_denoise::cloud_denoise_presets,
            page_denoise::denoise_chapter_local,
            page_denoise::cancel_denoise_local,
            page_denoise::benchmark_denoise_local,
            page_denoise::denoise_presets,
            page_denoise::replace_with_denoised,
            denoise_history::denoise_history,
            denoise_history::denoise_compare_image,
            inference::commands::write_inference_config,
            inference::commands::select_cloud_profile,
            inference::commands::store_cloud_secret,
            inference::commands::delete_cloud_secret,
            inference::commands::get_cloud_secret_summary,
            inference::commands::forget_cloud_secret_denials,
            inference::commands::check_cloud_connection,
            inference::commands::get_cloud_model_info,
            inference::commands::check_cloud_release,
            inference::gpu::get_cloud_gpu_status,
            inference::gpu::stop_cloud_gpu,
            inference::commands::prepare_cloud_consent,
            inference::commands::confirm_cloud_consent,
            inference::commands::submit_cloud_attempt,
            inference::commands::get_cloud_attempt_status,
            inference::commands::get_cloud_attempt_result,
            inference::review::resolve_qwen_review,
            inference::commands::cancel_cloud_attempt,
            inference::commands::reconcile_cloud_recovery,
            provision::run_cloud_provisioner,
            provision::provision_inspect,
            provision::provision_plan,
            provision::provision_apply,
            provision::provision_resume,
            provision::provision_cleanup,
            provision::provision_probe,
            provision::cancel_cloud_provisioner,
            library::list_projects,
            library::create_project,
            library::create_chapter,
            library::open_chapter,
            library::load_pages,
            history::history_load,
            history::history_push,
            history::history_move,
            library::rename_project,
            library::delete_project,
            library::delete_chapter,
            library::delete_mask,
            library::restore_region,
            library::set_layer_style,
            library::keep_dependency_result,
            library::set_detection_type,
            region::apply_tool,
            region::create_region,
            region::edit_detection_mask,
            region::set_detection_padding,
            region::preview_paint,
            region::rerun_mask,
            region::clean_anyway,
            region::sidecar_available,
            region::list_sidecar_models,
            screen_color::pick_screen_color,
            flux_install::install_flux_helper,
            exporting::export_chapter,
            exporting::plan_export_chapter,
            events::subscribe_events,
            events::unsubscribe_events,
            models::list_loaded_models,
            models::unload_model,
            models::list_accelerators,
            model_workflows::list_workflow_capabilities,
            model_workflows::import_full_rt,
            model_workflows::remove_full_rt,
            model_workflows::import_sam_ts,
            model_workflows::install_sam_ts,
            model_workflows::remove_sam_ts,
            model_workflows::verify_sam_ts,
            model_workflows::analyze_capabilities,
            model_workflows::analyze_chapter_page,
            model_workflows::cancel_capability_analysis,
            model_workflows::load_component_correction,
            model_workflows::prepare_component_write,
            model_workflows::apply_component_write,
            weights::list_models,
            weights::download_model,
            weights::download_model_group,
            weights::cancel_download,
            weights::delete_model,
            weights::delete_model_group,
            weights::discard_partial,
            weights::verify_model,
            weights::verify_model_group,
            weights::download_runtime,
            weights::delete_runtime,
            run::run_clean,
            run::cancel_run,
            run::resume_job,
            jobs::list_jobs,
            jobs::confirm_quit,
            jobs::hide_to_tray,
        ])
        // Pixels do not cross the command boundary. `tile.rs` resolves a chapter
        // to its job through the library and answers with whole PNG responses -
        // never a range, because WebView2 supports neither ranges nor streaming.
        // Asynchronous, so drawing a page never holds the UI thread.
        .register_asynchronous_uri_scheme_protocol(
            tile::SCHEME,
            tile::protocol(|app, chapter_id| {
                library::resolve_chapter(app, chapter_id).map_err(|error| error.to_string())
            }),
        )
        .build(app_context())
        .expect("error while building the application")
        .run(|app, event| {
            // A Dock click on macOS with the window hidden in the tray.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_main(app);
            }
            // A quit asked for in code (the tray's Quit, the app menu's, the
            // process plugin's `exit`) while jobs run is held and asked about.
            // `None` is the last window already gone, where there is nothing
            // left to ask in; a restart cannot be held.
            if let tauri::RunEvent::ExitRequested { code: Some(code), api, .. } = &event {
                if *code != tauri::RESTART_EXIT_CODE && hold_quit(app) {
                    api.prevent_exit();
                }
            }
            // The provisioner helper leads its own process group, so nothing
            // ends it when this process ends. A quit during cloud setup stops
            // it here rather than leaving it deploying with no one to read the
            // result; its journal makes a later resume safe.
            if let tauri::RunEvent::Exit = event {
                provision::stop_helpers_for_exit();
            }
        });
}

/// Whether a quit asked for now must wait for the user: jobs are running and
/// the quit is not yet confirmed. When it must, the window is brought back and
/// asked (`app://quit-requested`); see [`jobs`].
fn hold_quit<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> bool {
    let Some(running) = jobs::quit_held() else { return false };
    show_main(app);
    jobs::ask_quit(app, running, app.tray_by_id(TRAY_ID).is_some());
    true
}

/// The default macOS app menu's Quit is AppKit's `terminate:`, which ends the
/// process without an `ExitRequested` the guard could hold. It is swapped for
/// an item of our own on the same Cmd+Q that asks for the exit in code.
#[cfg(target_os = "macos")]
fn guard_menu_quit(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind};
    const QUIT_ID: &str = "app-quit";
    let menu = Menu::default(app)?;
    let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() else { return Ok(()) };
    let quit = app_menu.items()?.into_iter().rev().find_map(|item| match item {
        MenuItemKind::Predefined(item) if item.text().is_ok_and(|text| text.starts_with("Quit")) => Some(item),
        _ => None,
    });
    let Some(quit) = quit else { return Ok(()) };
    app_menu.remove(&quit)?;
    app_menu.append(&MenuItem::with_id(app, QUIT_ID, "Quit Manga Cleaner", true, Some("CmdOrCtrl+Q"))?)?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id().as_ref() == QUIT_ID { app.exit(0); }
    });
    Ok(())
}

/// Bring the main window back from the tray, the Dock, or a second launch,
/// including when it was minimised before it was hidden.
fn show_main<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
