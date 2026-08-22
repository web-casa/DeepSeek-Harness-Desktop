fn main() {
    println!("cargo:rerun-if-env-changed=STORE_BUILD");
    println!("cargo:rustc-check-cfg=cfg(store_build)");
    if std::env::var("STORE_BUILD").as_deref() == Ok("1") {
        println!("cargo:rustc-cfg=store_build");
    }
    // App-level ACL: only windows whose capability grants the generated
    // `allow-<command>` permissions may invoke these commands. The remote
    // Harness WebView ("harness" window) has an empty capability set, so it
    // gets zero IPC surface even though it renders in the same app.
    let manifest = tauri_build::AppManifest::new().commands(&[
        "get_status",
        "get_logs",
        "get_versions",
        "get_diagnostics",
        "get_diagnostic_mode",
        "set_diagnostic_mode",
        "clear_diagnostic_logs",
        "get_presentation_locale",
        "set_presentation_locale",
        "restart",
        "shutdown",
        "open_harness",
        "check_update",
        "install_update_and_restart",
        "export_diagnostics",
        "cancel_diagnostics_export",
        "quit_app",
        "list_user_presets",
        "preview_preset",
        "cancel_preset_preview",
        "import_preset",
        "export_preset",
        "delete_preset",
        "list_plugins",
        "preview_profile_patch_cleanup",
        "apply_profile_patch_cleanup",
        "install_plugin",
        "uninstall_plugin",
        "cancel_plugin_op",
        "get_plugin_recovery",
        "begin_plugin_recovery",
        "rollback_plugin_recovery",
        "finalize_plugin_recovery",
        "get_pending_plugin_install",
        "dismiss_pending_plugin_install",
        "get_pending_remote_preset",
        "dismiss_remote_preset",
        "confirm_remote_preset_download",
        "import_remote_preset",
        "pick_sideload_file",
        "market_search",
        "market_plugin",
        "market_image",
        "market_prepare_install",
        "market_install_plugin",
        "activate_market_plugin",
        "sideload_plugin",
    ]);
    let result = tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest));
    if let Err(e) = result {
        eprintln!("failed to run tauri-build: {e}");
        std::process::exit(1);
    }
}
