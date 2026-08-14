// Prevents an extra console window on Windows in release builds; dev builds
// keep it for `cargo run` logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

mod commands;
mod harness;
mod paths;

fn main() {
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.on_window_event(|window, event| {
        if window.label() == "bootstrap" {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        }
    });

    let builder = builder
        // Second launch focuses the existing windows instead of booting a
        // second Harness tree.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            for label in ["bootstrap", "harness"] {
                if let Some(win) = app.get_webview_window(label) {
                    let _ = win.show();
                    let _ = win.unminimize();
                    let _ = win.set_focus();
                }
            }
        }))
        // Persists/restores window size & position automatically.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            harness::init(&app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_logs,
            commands::get_versions,
            commands::get_diagnostics,
            commands::restart,
            commands::shutdown,
            commands::open_harness
        ]);

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("error while building DeepSeek Harness Desktop: {e}");
            std::process::exit(1);
        }
    };
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            // The sidecar kills the whole Node/Harness tree on stdin EOF,
            // and the Windows Job Object guarantees cleanup even if we
            // crash. This is the polite path.
            harness::shutdown_blocking(app);
        }
    });
}
