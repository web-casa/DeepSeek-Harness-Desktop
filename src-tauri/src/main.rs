// Prevents an extra console window on Windows in release builds; dev builds
// keep it for `cargo run` logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

mod commands;
mod harness;
mod paths;
mod tray;

fn main() {
    let builder = tauri::Builder::default();

    // Close-to-tray: when the tray is available, closing any window hides it
    // and the app keeps running in the tray; without a tray, per-platform
    // defaults remain (macOS hides bootstrap, Win/Linux quit on close) so a
    // hidden app can never become unreachable.
    let builder = builder.on_window_event(|window, event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            let app = window.app_handle();
            let tray_ok = tray::available(app);
            let ours = matches!(window.label(), "bootstrap" | "harness");
            if tray_ok && ours {
                api.prevent_close();
                let _ = window.hide();
            } else {
                #[cfg(target_os = "macos")]
                if window.label() == "bootstrap" {
                    api.prevent_close();
                    let _ = window.hide();
                }
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
        // Persist window size/position but NOT visibility: after hide→quit→
        // relaunch the bootstrap window must always come back.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        - tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .setup(|app| {
            harness::init(&app.handle().clone());
            tray::init(&app.handle().clone());
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
    app.run(|app, event| match event {
        // macOS Dock icon click: restore the bootstrap window only when
        // nothing is visible (never steal focus from an open Harness window).
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen {
            has_visible_windows,
            ..
        } => {
            if !has_visible_windows {
                if let Some(win) = app.get_webview_window("bootstrap") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
        }
        tauri::RunEvent::Exit => {
            // The sidecar kills the whole Node/Harness tree on stdin EOF,
            // and the Windows Job Object guarantees cleanup even if we
            // crash. This is the polite path.
            harness::shutdown_blocking(app);
        }
        _ => {}
    });
}
