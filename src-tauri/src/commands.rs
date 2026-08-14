//! IPC commands for the bootstrap window.
//!
//! Every command here is permission-gated by the app ACL (see build.rs):
//! only the local "bootstrap" window has the `allow-*` grants; the remote
//! Harness WebView has an empty capability set and cannot invoke anything.

use crate::harness::{
    child_alive, open_harness_window, reset_restart_attempts, respawn_sidecar, send_raw,
    snapshot_payload, Runtime, CMD_ID_RESTART, CMD_ID_SHUTDOWN,
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
pub fn restart(runtime: State<'_, Runtime>, app: AppHandle) -> Result<(), String> {
    // Sidecar died: relaunch the whole chain instead of asking the user to
    // restart the app.
    if !child_alive(&runtime) {
        let respawned = respawn_sidecar(&app);
        if let Err(e) = respawned {
            let mut s = runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            s.last_error = Some(e.clone());
            s.status = crate::harness::Status::Crashed;
            return Err(e);
        }
        reset_restart_attempts(&runtime);
        let mut s = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        s.last_error = None;
        return Ok(());
    }

    if let Err(error) = send_raw(
        &runtime,
        &serde_json::json!({"id": CMD_ID_RESTART, "command": "restart"}),
    ) {
        let mut s = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        s.last_error = Some(error.clone());
        s.status = crate::harness::Status::Crashed;
        return Err(error);
    }

    reset_restart_attempts(&runtime);
    let mut s = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    s.last_error = None;
    s.status = crate::harness::Status::Starting;
    Ok(())
}

#[tauri::command]
pub fn shutdown(runtime: State<'_, Runtime>) -> Result<(), String> {
    if !child_alive(&runtime) {
        // Keep the real status (Stopped/Idle stays what it is) — only the
        // message explains why nothing happened.
        let error = "sidecar 未运行，无需停止".to_string();
        runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last_error = Some(error.clone());
        return Err(error);
    }

    if let Err(error) = send_raw(
        &runtime,
        &serde_json::json!({"id": CMD_ID_SHUTDOWN, "command": "shutdown"}),
    ) {
        let mut s = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        s.last_error = Some(error.clone());
        s.status = crate::harness::Status::Crashed;
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
