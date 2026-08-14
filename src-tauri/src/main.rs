// Prevents an extra console window on Windows in release builds; dev builds
// keep it for `cargo run` logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod harness;
mod paths;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            harness::init(&app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_logs,
            commands::get_versions,
            commands::restart,
            commands::shutdown,
            commands::open_harness
        ])
        .build(tauri::generate_context!())
        .expect("error while building DeepSeek Harness Desktop")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // The sidecar kills the whole Node/Harness tree on stdin EOF,
                // and the Windows Job Object guarantees cleanup even if we
                // crash. This is the polite path.
                harness::shutdown_blocking(app);
            }
        });
}
