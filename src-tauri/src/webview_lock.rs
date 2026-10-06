//! Disable browser chrome actions that would discard the desktop editor state.
pub fn install(app: &tauri::AppHandle) {
    #[cfg(windows)]
    {
        use tauri::Manager;
        use windows_core::Interface;
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.with_webview(|webview| unsafe {
                if let Ok(settings) = webview.controller().CoreWebView2().and_then(|view| view.Settings()) {
                    let _ = settings.SetAreDefaultContextMenusEnabled(false);
                    if let Ok(settings) = settings.cast::<ICoreWebView2Settings3>() {
                        let _ = settings.SetAreBrowserAcceleratorKeysEnabled(false);
                    }
                }
            });
        }
    }
    // macOS and Linux use the app-wide contextmenu and keyboard preventDefault
    // listeners; custom editor context menus still receive their events.
    #[cfg(not(windows))]
    let _ = app;
}
