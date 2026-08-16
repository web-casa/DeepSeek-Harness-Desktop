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
    let dsh_home = s.dsh_home.clone().unwrap_or_default();
    // The clipboard path must get the same best-effort redaction as the zip
    // export — a copy-paste of raw harness logs would leak exactly what
    // export_diagnostics masks.
    let logs_tail: Vec<(String, String)> = s.logs[tail_start..]
        .iter()
        .map(|(stream, line)| (stream.clone(), redact(line, &dsh_home)))
        .collect();
    serde_json::json!({
        "status": s.status,
        "url": s.url,
        "pid": s.pid,
        "lastError": s.last_error.as_deref().map(|e| redact(e, &dsh_home)),
        "versions": s.versions,
        "dshHome": s.dsh_home,
        "platform": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "logsTail": logs_tail,
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
    // macOS updater is deliberately OFF until signing + notarization land
    // (Gatekeeper rejects un-notarized updates) — report it as unsupported,
    // NOT as "already up to date".
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Ok(serde_json::json!({ "available": false, "unsupported": true }))
    }
    #[cfg(target_os = "windows")]
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
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Err("updates are not supported on this platform".to_string())
    }
    #[cfg(target_os = "windows")]
    {
        use tauri_plugin_updater::UpdaterExt;
        let updater = app.updater().map_err(|e| e.to_string())?;
        let update = updater
            .check()
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no update available".to_string())?;
        update
            .download_and_install(
                |chunk, total| {
                    use tauri::Emitter;
                    let _ = app.emit(
                        "update-progress",
                        serde_json::json!({ "downloaded": chunk, "total": total }),
                    );
                },
                || {},
            )
            .await
            .map_err(|e| format!("update install failed: {e}"))?;
        // AppHandle::restart is `-> !` on EVERY platform (spawn + exit(0));
        // there is no success tail — the process restarts in place.
        app.restart();
    }
}

// ---------------------------------------------------------------------------
// Diagnostics export (best-effort redaction) and app quit.
// ---------------------------------------------------------------------------

/// Mask the most common secret shapes. Best effort only: harness logs can
/// contain arbitrary tool output, so this must never be described as "safe".
///
/// Content is copied char-by-char; the byte view is used ONLY to recognize
/// the ASCII secret prefixes, and it is only consulted at char boundaries,
/// so multi-byte UTF-8 (e.g. Chinese log lines) passes through intact.
/// The `[n..]` slices after a mask are safe because the counted token bytes
/// are ASCII: the slice always lands on a char boundary.
fn redact(text: &str, dsh_home: &str) -> String {
    // Guard the empty case: `str::replace("", …)` interleaves the mask
    // between every character instead of matching nothing.
    let out = if dsh_home.is_empty() {
        text.to_owned()
    } else {
        text.replace(dsh_home, "<DSH_HOME>")
    };
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while !rest.is_empty() {
        let bytes = rest.as_bytes();
        // sk- + 16+ alphanumerics
        if bytes.starts_with(b"sk-") {
            let j = bytes
                .iter()
                .skip(3)
                .take_while(|b| b.is_ascii_alphanumeric())
                .count();
            if j >= 16 {
                result.push_str("sk-***");
                rest = &rest[3 + j..];
                continue;
            }
        }
        // Bearer <token>
        if bytes.starts_with(b"Bearer ") {
            let j = bytes
                .iter()
                .skip(7)
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
                .count();
            if j >= 8 {
                result.push_str("Bearer ***");
                rest = &rest[7 + j..];
                continue;
            }
        }
        // AKIA + 12+ uppercase/digits (AWS access key id)
        if bytes.starts_with(b"AKIA") {
            let j = bytes
                .iter()
                .skip(4)
                .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                .count();
            if j >= 12 {
                result.push_str("AKIA***");
                rest = &rest[4 + j..];
                continue;
            }
        }
        // Plain content: copy whole chars, never split UTF-8.
        match rest.chars().next() {
            Some(ch) => {
                result.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
            None => break, // rest was just checked non-empty; unreachable
        }
    }
    result
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

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn masks_sk_tokens_16_plus_alnum() {
        assert_eq!(redact("sk-abcdefghijklmnop", ""), "sk-***");
        assert_eq!(redact("sk-0123456789abcdef", ""), "sk-***");
        // 15 chars: below threshold, left untouched.
        assert_eq!(redact("sk-abcdefghijklmno", ""), "sk-abcdefghijklmno");
    }

    #[test]
    fn masks_bearer_tokens() {
        assert_eq!(redact("Bearer abcdefgh", ""), "Bearer ***");
        assert_eq!(redact("Bearer ab-cd_ef", ""), "Bearer ***");
        // 5 chars: below threshold, left untouched.
        assert_eq!(redact("Bearer short", ""), "Bearer short");
    }

    #[test]
    fn masks_aws_access_key_ids() {
        assert_eq!(redact("AKIAABCDEFGHIJKLMN", ""), "AKIA***");
        assert_eq!(redact("AKIAABCDEFGHIJK1", ""), "AKIA***");
        // 11 chars after the prefix: below threshold, left untouched.
        assert_eq!(redact("AKIAABCDEFGHIJK", ""), "AKIAABCDEFGHIJK");
    }

    #[test]
    fn redacts_dsh_home_path() {
        assert_eq!(redact("/home/u/.dsh/log", "/home/u/.dsh"), "<DSH_HOME>/log");
    }

    #[test]
    fn empty_dsh_home_is_a_noop_not_a_mangler() {
        // `str::replace("", …)` would interleave the mask between chars.
        assert_eq!(redact("abc", ""), "abc");
    }

    #[test]
    fn preserves_multibyte_utf8_and_masks_adjacent_secrets() {
        // Regression for the byte-wise scan that pushed every UTF-8 byte as
        // a lone `char`, doubling Chinese text into mojibake on every export.
        let input = "日志：sk-abcdefghijklmnop 正常退出，Bearer 12345678，路径中文";
        assert_eq!(
            redact(input, ""),
            "日志：sk-*** 正常退出，Bearer ***，路径中文"
        );
    }

    #[test]
    fn secret_directly_after_a_multibyte_char() {
        // The ASCII prefix starts right after a 3-byte char; the match must
        // still be found (and the char must survive intact).
        assert_eq!(redact("中sk-abcdefghijklmnop", ""), "中sk-***");
    }

    #[test]
    fn redaction_is_idempotent() {
        // Masked forms ("sk-***" etc.) contain no alnum run after the prefix,
        // so a second pass must not change anything.
        let once = redact("sk-abcdefghijklmnop Bearer abcdefgh 中文", "");
        assert_eq!(redact(&once, ""), once);
    }
}

// ---------------------------------------------------------------------------
// Preset (.dshpreset) transfer — the shell-side safe boundary. See
// preset.rs for the security rationale; upstream is never patched.
// ---------------------------------------------------------------------------

/// The preview pending user confirmation: (archive path, inspection result).
pub struct PendingPreset(
    pub std::sync::Mutex<Option<(std::path::PathBuf, crate::preset::ArchivePreview)>>,
);

#[tauri::command]
pub fn list_user_presets(runtime: State<'_, Runtime>) -> Vec<String> {
    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone()
        .unwrap_or_default();
    let root = crate::preset::user_preset_root(std::path::Path::new(&dsh_home));
    let mut ids = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let is_dir = entry
                .file_type()
                .map(|t| t.is_dir() && !t.is_symlink())
                .unwrap_or(false);
            if crate::preset::is_valid_preset_id(&name) && is_dir {
                ids.push(name);
            }
        }
    }
    ids.sort();
    ids
}

/// Pick an archive, validate it, and hold the result for confirmation.
#[tauri::command]
pub async fn preview_preset(
    app: AppHandle,
    pending: State<'_, PendingPreset>,
) -> Result<Value, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = std::sync::mpsc::channel::<Option<std::path::PathBuf>>();
    app.dialog()
        .file()
        .add_filter("dshpreset", &["dshpreset"])
        .pick_file(move |p| {
            let _ = tx.send(p.map(|f| f.into_path().unwrap_or_default()));
        });
    let Some(path) = rx.recv().map_err(|e| e.to_string())? else {
        return Err("cancelled".to_string());
    };
    let preview = crate::preset::inspect_archive(&path)?;
    let json = serde_json::json!({
        "id": preview.id,
        "files": preview.files,
        "warnings": preview.warnings,
    });
    *pending
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((path, preview));
    Ok(json)
}

/// Install the previously previewed archive (two-phase: the confirmation
/// dialog sits between preview_preset and this call).
#[tauri::command]
pub fn import_preset(
    runtime: State<'_, Runtime>,
    pending: State<'_, PendingPreset>,
) -> Result<String, String> {
    // Clone, not take(): a failed import must keep the preview so the user
    // can see the error and retry without re-picking the file.
    let held = pending
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .cloned();
    let Some((path, _)) = held else {
        return Err("no previewed archive — run preview first".to_string());
    };
    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;
    match crate::preset::install_archive(&path, std::path::Path::new(&dsh_home)) {
        Ok(id) => {
            *pending
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            Ok(id)
        }
        Err(e) => Err(e),
    }
}

/// Export one user-authored preset to a user-chosen location.
#[tauri::command]
pub async fn export_preset(
    app: AppHandle,
    id: String,
    runtime: State<'_, Runtime>,
) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = std::sync::mpsc::channel::<Option<std::path::PathBuf>>();
    app.dialog()
        .file()
        .add_filter("dshpreset", &["dshpreset"])
        .set_file_name(format!("{id}.dshpreset"))
        .save_file(move |p| {
            let _ = tx.send(p.map(|f| f.into_path().unwrap_or_default()));
        });
    let Some(dest) = rx.recv().map_err(|e| e.to_string())? else {
        return Err("cancelled".to_string());
    };
    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;
    crate::preset::export_preset(&id, std::path::Path::new(&dsh_home), &dest)
}
