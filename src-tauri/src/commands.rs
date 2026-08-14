//! IPC commands for the bootstrap window.
//!
//! Every command here is permission-gated by the app ACL (see build.rs):
//! only the local "bootstrap" window has the `allow-*` grants; the remote
//! Harness WebView has an empty capability set and cannot invoke anything.

use crate::harness::{
    child_alive, reset_restart_attempts, send_raw, snapshot_payload, Runtime,
};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub fn get_status(runtime: State<'_, Runtime>) -> Value {
    snapshot_payload(&runtime.state)
}

#[tauri::command]
pub fn get_diagnostics(runtime: State<'_, Runtime>) -> Value {
    let s = runtime.state.lock().unwrap();
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
    runtime.state.lock().unwrap().logs.clone()
}

#[tauri::command]
pub fn get_versions(runtime: State<'_, Runtime>) -> Value {
    serde_json::to_value(&runtime.state.lock().unwrap().versions).unwrap_or(Value::Null)
}

#[tauri::command]
pub fn restart(runtime: State<'_, Runtime>) -> Result<(), String> {
    if !child_alive(&runtime) {
        let error = "sidecar 未运行".to_string();
        let mut s = runtime.state.lock().unwrap();
        s.last_error = Some(error.clone());
        s.status = crate::harness::Status::Crashed;
        return Err(error);
    }

    if let Err(error) = send_raw(
        &runtime,
        &serde_json::json!({"id": 100, "command": "restart"}),
    ) {
        let mut s = runtime.state.lock().unwrap();
        s.last_error = Some(error.clone());
        s.status = crate::harness::Status::Crashed;
        return Err(error);
    }

    reset_restart_attempts(&runtime);
    let mut s = runtime.state.lock().unwrap();
    s.last_error = None;
    s.status = crate::harness::Status::Starting;
    Ok(())
}

#[tauri::command]
pub fn shutdown(runtime: State<'_, Runtime>) -> Result<(), String> {
    if !child_alive(&runtime) {
        let error = "sidecar 未运行".to_string();
        let mut s = runtime.state.lock().unwrap();
        s.last_error = Some(error.clone());
        s.status = crate::harness::Status::Crashed;
        return Err(error);
    }

    if let Err(error) = send_raw(
        &runtime,
        &serde_json::json!({"id": 101, "command": "shutdown"}),
    ) {
        let mut s = runtime.state.lock().unwrap();
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
        .unwrap()
        .url
        .clone()
        .ok_or_else(|| "Harness 尚未就绪".to_string())?;
    let parsed = tauri::Url::parse(&url).map_err(|e| format!("无效 URL: {e}"))?;
    let app = app.clone();
    let app_in = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app_in.get_webview_window("harness") {
            let _ = win.navigate(parsed.clone());
            let _ = win.show();
            let _ = win.set_focus();
        } else {
            let _ = tauri::WebviewWindowBuilder::new(
                &app_in,
                "harness",
                tauri::WebviewUrl::External(parsed),
            )
            .title("DeepSeek Harness")
            .inner_size(1280.0, 800.0)
            .min_inner_size(960.0, 600.0)
            .build();
        }
    });
    Ok(())
}
