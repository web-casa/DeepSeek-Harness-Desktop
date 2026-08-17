// Prevents an extra console window on Windows in release builds; dev builds
// keep it for `cargo run` logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

mod commands;
mod deep_link;
mod harness;
mod paths;
mod plugins;
mod preset;
mod tray;

fn main() {
    // Windows: give the shell a private hidden console before any plugin
    // child exists. Plugin children are spawned WITHOUT CREATE_NO_WINDOW so
    // they inherit it — that inheritance is what makes PlatformChild's
    // graceful() able to deliver CTRL_C to the whole tree (the sidecar does
    // the same for the Harness tree). No-op on unix.
    dsh_sidecar::platform::ensure_hidden_console();

    let builder = tauri::Builder::default();

    // Close-to-tray: when the tray is available, closing any window hides it
    // and the app keeps running in the tray. Without a tray there is no
    // background-resident mode: macOS hides bootstrap (native convention,
    // quit via Cmd+Q), Windows/Linux quit the whole app when bootstrap is
    // closed — otherwise a running Harness would be left with its only
    // control surface destroyed and no way to recover it in-session.
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
                #[cfg(not(target_os = "macos"))]
                if window.label() == "bootstrap" {
                    // No tray: bootstrap is the app's control surface — its
                    // close quits the app (graceful: RunEvent::Exit runs
                    // shutdown_blocking) instead of stranding the harness.
                    app.exit(0);
                }
            }
        }
    });

    let builder = builder
        // Second launch focuses the existing windows instead of booting a
        // second Harness tree.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Normal second launch keeps the current behavior (Harness on
            // top). A deep-link second launch must finish with bootstrap
            // focused: the confirmation dialog lives there, and process_urls
            // reveals it BEFORE this callback runs.
            let deep_link = argv.iter().any(|arg| {
                arg.get(.."dsharness://".len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("dsharness://"))
            });
            let order: &[&str] = if deep_link {
                &["harness", "bootstrap"]
            } else {
                &["bootstrap", "harness"]
            };
            for label in order {
                if let Some(win) = app.get_webview_window(label) {
                    let _ = win.show();
                    let _ = win.unminimize();
                    let _ = win.set_focus();
                }
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
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
            // Tray first: harness init failure paths publish snapshots that
            // must reach the tray status line.
            tray::init(&app.handle().clone());
            harness::init(&app.handle().clone());
            commands::sweep_remote_preset_temp(&app.state::<harness::Runtime>());
            // The two-phase preset import holds its preview here between
            // preview_preset and import_preset. MUST be managed: extracting
            // an unmanaged State panics at the first command invocation.
            app.manage(commands::PendingPreset(std::sync::Mutex::new(None)));
            app.manage(std::sync::Arc::new(plugins::PluginRunner::new()));
            // Deep-link parsing/dispatch. Manage the pending-request slot
            // BEFORE init drains get_current(): a cold start URL can arrive
            // before the webview subscribed to plugin-install-request.
            app.manage(deep_link::PendingPluginInstall::default());
            app.manage(deep_link::PendingRemotePreset::default());
            app.manage(deep_link::InstallArbiter::default());
            deep_link::init(app.handle());
            Ok(())
        })
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_logs,
            commands::get_versions,
            commands::get_diagnostics,
            commands::restart,
            commands::shutdown,
            commands::open_harness,
            commands::check_update,
            commands::install_update_and_restart,
            commands::export_diagnostics,
            commands::quit_app,
            commands::list_user_presets,
            commands::preview_preset,
            commands::import_preset,
            commands::export_preset,
            commands::delete_preset,
            commands::list_plugins,
            commands::install_plugin,
            commands::uninstall_plugin,
            commands::cancel_plugin_op,
            commands::get_pending_plugin_install,
            commands::dismiss_pending_plugin_install,
            commands::get_pending_remote_preset,
            commands::dismiss_remote_preset,
            commands::confirm_remote_preset_download,
            commands::import_remote_preset
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
            commands::sweep_remote_preset_temp(&app.state::<harness::Runtime>());
            // Kill a running `dsh plugin` tree first: it is a separate
            // process group / Job Object from the sidecar's Harness tree,
            // and on unix it would be orphaned once the shell exits.
            app.state::<std::sync::Arc<plugins::PluginRunner>>()
                .shutdown();
            // The sidecar kills the whole Node/Harness tree on stdin EOF,
            // and the Windows Job Object guarantees cleanup even if we
            // crash. This is the polite path.
            harness::shutdown_blocking(app);
        }
        _ => {}
    });
}
