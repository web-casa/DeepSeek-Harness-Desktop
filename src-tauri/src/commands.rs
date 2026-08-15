//! IPC commands for the bootstrap window.
//!
//! Every command here is permission-gated by the app ACL (see build.rs):
//! only the local "bootstrap" window has the `allow-*` grants; the remote
//! Harness WebView has an empty capability set and cannot invoke anything.

use crate::harness::{
    child_alive, open_harness_window, publish_snapshot, request_restart, send_raw,
    snapshot_payload, Runtime, CMD_ID_SHUTDOWN,
};
use serde_json::Value;
use tauri::{AppHandle, State};

#[tauri::command]
pub fn get_status(runtime: State<'_, Runtime>) -> Value {
    snapshot_payload(&runtime.state)
}

#[tauri::command]
pub fn get_diagnostics(runtime: State<'_, Runtime>) -> Value {
    let s = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tail_start = s.logs.len().saturating_sub(200);
    serde_json::json!({
        "status": s.status,
        "url": s.url,
        "pid": s.pid,
        "lastError": s.last_error,
        "versions": s.versions,
        "dshHome": s.dsh_home,
        "platform": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "logsTail": &s.logs[tail_start..],
    })
}

#[tauri::command]
pub fn get_logs(runtime: State<'_, Runtime>) -> Vec<(String, String)> {
    runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .logs
        .clone()
}

#[tauri::command]
pub fn get_versions(runtime: State<'_, Runtime>) -> Value {
    serde_json::to_value(
        &runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .versions,
    )
    .unwrap_or(Value::Null)
}

#[tauri::command]
pub fn restart(_runtime: State<'_, Runtime>, app: AppHandle) -> Result<(), String> {
    // Unified restart: sidecar alive → restart command; dead → full respawn.
    request_restart(&app)
}

#[tauri::command]
pub fn shutdown(runtime: State<'_, Runtime>, app: AppHandle) -> Result<(), String> {
    if !child_alive(&runtime) {
        // Keep the real status (Stopped/Idle stays what it is) — only the
        // message explains why nothing happened.
        let error = "sidecar 未运行，无需停止".to_string();
        runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last_error = Some(error.clone());
        publish_snapshot(&app, &runtime.state);
        return Err(error);
    }

    if let Err(error) = send_raw(
        &runtime,
        &serde_json::json!({"id": CMD_ID_SHUTDOWN, "command": "shutdown"}),
    ) {
        {
            // Scope the lock: publish_snapshot takes the same mutex and a
            // held guard here would deadlock the command.
            let mut s = runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            s.last_error = Some(error.clone());
            s.status = crate::harness::Status::Crashed;
        }
        publish_snapshot(&app, &runtime.state);
        return Err(error);
    }

    Ok(())
}

#[tauri::command]
pub fn open_harness(runtime: State<'_, Runtime>, app: AppHandle) -> Result<(), String> {
    let url = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .url
        .clone()
        .ok_or_else(|| "Harness 尚未就绪".to_string())?;
    open_harness_window(&app, &url);
    Ok(())
}

// ---------------------------------------------------------------------------
// Updater (Windows for now; macOS activates once signed + notarized).
// The update package's authenticity is enforced by the minisign pubkey
// embedded at build time (independent of app code signing).
// ---------------------------------------------------------------------------

/// Result of a silent update check, surfaced to the bootstrap UI.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Value, String> {
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = app;
        Ok(serde_json::json!({ "available": false, "unsupported": true }))
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        use tauri_plugin_updater::UpdaterExt;
        let updater = app.updater().map_err(|e| e.to_string())?;
        match updater.check().await {
            Ok(Some(update)) => Ok(serde_json::json!({
                "available": true,
                "version": update.version,
                "notes": update.body.clone().unwrap_or_default(),
            })),
            Ok(None) => Ok(serde_json::json!({ "available": false })),
            Err(e) => Err(format!("update check failed: {e}")),
        }
    }
}

/// Download and install the latest update, then restart the app.
#[tauri::command]
pub async fn install_update_and_restart(app: AppHandle) -> Result<(), String> {
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = app;
        Err("updates are not supported on this platform".to_string())
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        use tauri_plugin_updater::UpdaterExt;
        let updater = app.updater().map_err(|e| e.to_string())?;
        let update = updater
            .check()
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no update available".to_string())?;
        update
            .download_and_install(|_chunk, _total| {}, || {})
            .await
            .map_err(|e| format!("update install failed: {e}"))?;
        app.restart();
        Ok(())
    }
}
