fn main() {
    // App-level ACL: only windows whose capability grants the generated
    // `allow-<command>` permissions may invoke these commands. The remote
    // Harness WebView ("harness" window) has an empty capability set, so it
    // gets zero IPC surface even though it renders in the same app.
    let manifest = tauri_build::AppManifest::new().commands(&[
        "get_status",
        "get_logs",
        "get_versions",
        "get_diagnostics",
        "restart",
        "shutdown",
        "open_harness",
        "check_update",
        "install_update_and_restart",
        "export_diagnostics",
        "quit_app",
        "list_user_presets",
        "preview_preset",
        "import_preset",
        "export_preset",
    ]);
    let result = tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest));
    if let Err(e) = result {
        eprintln!("failed to run tauri-build: {e}");
        std::process::exit(1);
    }
}
