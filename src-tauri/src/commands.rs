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
        // `restart` DIVERGES on Windows (the process is restarted in place),
        // so a shared success tail would be unreachable there and trip
        // clippy's unreachable_code under -D warnings. cfg the tail instead.
        #[cfg(target_os = "windows")]
        app.restart();
        #[cfg(not(target_os = "windows"))]
        {
            app.restart();
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Diagnostics export (best-effort redaction) and app quit.
// ---------------------------------------------------------------------------

/// Mask the most common secret shapes. Best effort only: harness logs can
/// contain arbitrary tool output, so this must never be described as "safe".
fn redact(text: &str, dsh_home: &str) -> String {
    let mut out = text.replace(dsh_home, "<DSH_HOME>");
    let mut i = 0usize;
    let bytes = out.as_bytes();
    let mut result = String::with_capacity(out.len());
    while i < bytes.len() {
        let rest = &bytes[i..];
        // sk- + 16+ alphanumerics
        if rest.starts_with(b"sk-") {
            let j = rest
                .iter()
                .skip(3)
                .take_while(|b| b.is_ascii_alphanumeric())
                .count();
            if j >= 16 {
                result.push_str("sk-***");
                i += 3 + j;
                continue;
            }
        }
        // Bearer <token>
        if rest.starts_with(b"Bearer ") {
            let j = rest
                .iter()
                .skip(7)
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
                .count();
            if j >= 8 {
                result.push_str("Bearer ***");
                i += 7 + j;
                continue;
            }
        }
        // AKIA + 16 uppercase (AWS access key id)
        if rest.starts_with(b"AKIA") {
            let j = rest
                .iter()
                .skip(4)
                .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                .count();
            if j >= 12 {
                result.push_str("AKIA***");
                i += 4 + j;
                continue;
            }
        }
        // Token-ish assignment: <name>=<20+ alnum> where name hints a secret
        result.push(bytes[i] as char);
        i += 1;
    }
    out = result;
    out
}

/// Write a diagnostics zip (status/versions/log tail) to a user-chosen path.
/// Logs come from the sidecar's in-memory ring ONLY — DSH_HOME disk files
/// (sessions etc.) are deliberately out of scope.
#[tauri::command]
pub async fn export_diagnostics(app: AppHandle, runtime: State<'_, Runtime>) -> Result<(), String> {
    use std::io::Write;
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = std::sync::mpsc::channel::<Option<std::path::PathBuf>>();
    app.dialog()
        .file()
        .add_filter("ZIP", &["zip"])
        .set_file_name("dsd-diagnostics.zip")
        .save_file(move |path| {
            let _ = tx.send(path.map(|p| p.into_path().unwrap_or_default()));
        });
    let Some(path) = rx.recv().map_err(|e| e.to_string())? else {
        return Err("save dialog cancelled".to_string());
    };

    let s = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dsh_home = s.dsh_home.clone().unwrap_or_default();
    let tail_start = s.logs.len().saturating_sub(500);
    let payload = serde_json::json!({
        "generator": "deepseek-harness-desktop",
        "status": s.status,
        "pid": s.pid,
        "versions": s.versions,
        "platform": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "lastError": s.last_error,
        "logsTail": s.logs[tail_start..].iter().map(|(stream, line)| {
            serde_json::json!({ "stream": stream, "line": redact(line, &dsh_home) })
        }).collect::<Vec<_>>(),
    });
    let mut text = serde_json::to_string_pretty(&payload).unwrap_or_default();
    text = redact(&text, &dsh_home);

    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("diagnostics.json", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// Exit the whole desktop app (graceful shutdown runs in RunEvent::Exit).
#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}
