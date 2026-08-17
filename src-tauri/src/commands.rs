//! IPC commands for the bootstrap window.
//!
//! Every command here is permission-gated by the app ACL (see build.rs):
//! only the local "bootstrap" window has the `allow-*` grants; the remote
//! Harness WebView has an empty capability set and cannot invoke anything.

use crate::harness::{
    child_alive, open_harness_window, publish_snapshot, request_restart, send_raw,
    snapshot_payload, Runtime, CMD_ID_SHUTDOWN,
};
use dsh_sidecar::platform::{PlatformChild, SpawnSpec};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

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

    // Build the payload under the lock, then DROP the guard before the slow
    // zip write: holding it across disk I/O would stall the watcher's
    // publish_snapshot (status/tray updates) on a slow disk.
    let (payload, dsh_home) = {
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
        (payload, dsh_home)
    };
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{read_installed_plugins, redact};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dsd-plugin-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    #[test]
    fn plugin_list_reads_deps_and_versions_sorted() {
        let dir = temp_dir("list");
        std::fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"zz-top":"1.0.0","is-odd":"^3.0.1"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("node_modules").join("is-odd")).unwrap();
        std::fs::write(
            dir.join("node_modules").join("is-odd").join("package.json"),
            r#"{"name":"is-odd","version":"3.0.1"}"#,
        )
        .unwrap();
        let entries = read_installed_plugins(&dir);
        assert_eq!(
            entries,
            vec![
                ("is-odd".to_string(), "3.0.1".to_string()),
                // zz-top is a dependency but its tree entry is missing:
                // still listed, with an unresolved version marker.
                ("zz-top".to_string(), "—".to_string()),
            ]
        );
    }

    #[test]
    fn plugin_list_tolerates_missing_or_malformed_profile() {
        let dir = temp_dir("empty");
        assert_eq!(read_installed_plugins(&dir), Vec::<(String, String)>::new());
        std::fs::write(dir.join("package.json"), "not json").unwrap();
        assert_eq!(read_installed_plugins(&dir), Vec::<(String, String)>::new());
        std::fs::write(
            dir.join("package.json"),
            r#"{"dependencies":"not-an-object"}"#,
        )
        .unwrap();
        // Malformed dependency payloads are skipped safely.
        assert_eq!(read_installed_plugins(&dir), Vec::<(String, String)>::new());
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
pub fn list_user_presets(runtime: State<'_, Runtime>) -> Value {
    // Resolved paths only: Path::new("") would make read_dir resolve the
    // relative ".agent-presets" against the process CWD (review S4).
    let Some(paths) = runtime.paths() else {
        return serde_json::json!([]);
    };
    let rows = crate::preset::validate_user_presets(&paths.dsh_home);
    serde_json::json!(rows
        .into_iter()
        .map(|row| serde_json::json!({
            "id": row.id,
            "issues": row
                .issues
                .into_iter()
                .map(|(kind, detail)| serde_json::json!({
                    "kind": kind.as_str(),
                    "detail": detail,
                }))
                .collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

/// Delete one user preset (see preset::delete_preset for the symlink/id
/// refusal semantics).
#[tauri::command]
pub fn delete_preset(runtime: State<'_, Runtime>, id: String) -> Result<(), String> {
    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;
    crate::preset::delete_preset(&id, std::path::Path::new(&dsh_home))
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

// ---------------------------------------------------------------------------
// Plugin installation (bundled pnpm + official `dsh plugin` CLI). The whole
// node → dsh → pnpm → node-gyp tree runs under dsh-sidecar's PlatformChild
// (process group / Job Object), so cancel and app exit clean it fully.
// ---------------------------------------------------------------------------

/// Pure profile-dir parse (unit-tested): user-installed plugin names from
/// package.json dependencies, with resolved versions read from
/// node_modules/<pkg>/package.json ("—" when the tree entry is missing).
/// In-box bundles are not dependencies here, so they never appear — the UI
/// can therefore offer uninstall on every listed row (plan §P1.4).
fn read_installed_plugins(profile_dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(profile_dir.join("package.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(deps) = json.get("dependencies").and_then(|d| d.as_object()) {
                for (name, _spec) in deps {
                    let version = std::fs::read_to_string(
                        profile_dir
                            .join("node_modules")
                            .join(name)
                            .join("package.json"),
                    )
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|p| {
                        p.get("version")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "—".to_string());
                    out.push((name.clone(), version));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[tauri::command]
pub fn list_plugins(
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
) -> Value {
    let entries = runtime
        .paths()
        .map(|p| read_installed_plugins(&p.dsh_home.join("profiles").join("web")))
        .unwrap_or_default();
    serde_json::json!({
        "plugins": entries
            .into_iter()
            .map(|(name, version)| serde_json::json!({ "name": name, "version": version }))
            .collect::<Vec<_>>(),
        // The backend busy flag survives webview reloads; the UI must be
        // able to resync instead of showing a stale idle state while an op
        // is still running (single-flight is app-wide).
        "busy": plugins.busy.load(Ordering::SeqCst),
    })
}

fn run_plugin_op(
    app: AppHandle,
    paths: crate::paths::RuntimePaths,
    plugins: Arc<crate::plugins::PluginRunner>,
    name: String,
    op: &'static str,
) {
    use std::io::{BufRead, BufReader};

    let pnpm_cjs = paths
        .harness_dir
        .join("node_modules")
        .join("pnpm")
        .join("bin")
        .join("pnpm.cjs");
    let shim_dir = match crate::plugins::ensure_pnpm_shim(&paths.dsh_home, &paths.node, &pnpm_cjs) {
        Ok(d) => d,
        Err(e) => {
            let _ = app.emit("plugin-done", serde_json::json!({ "exit": 1, "tail": e }));
            plugins.busy.store(false, Ordering::SeqCst);
            return;
        }
    };
    let mut path_env = shim_dir.to_string_lossy().to_string();
    if let Some(old) = std::env::var_os("PATH") {
        path_env.push(std::path::MAIN_SEPARATOR);
        path_env.push_str(&old.to_string_lossy());
    }
    let spec = SpawnSpec {
        node: paths.node.to_string_lossy().to_string(),
        script: paths
            .harness_dir
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js")
            .to_string_lossy()
            .to_string(),
        args: vec![
            "plugin".to_string(),
            "--profile".to_string(),
            "web".to_string(),
            op.to_string(),
            name,
        ],
        cwd: paths.harness_dir.to_string_lossy().to_string(),
        env: vec![
            (
                "DSH_HOME".to_string(),
                paths.dsh_home.to_string_lossy().to_string(),
            ),
            ("PATH".to_string(), path_env),
        ],
    };
    let inherited = std::env::vars_os().collect::<Vec<_>>();
    let child = match PlatformChild::spawn(&spec, &inherited) {
        Ok(c) => c,
        Err(e) => {
            let _ = app.emit(
                "plugin-done",
                serde_json::json!({ "exit": 1, "tail": format!("spawn failed: {e}") }),
            );
            plugins.busy.store(false, Ordering::SeqCst);
            return;
        }
    };
    // Close the spawn/store race against RunEvent::Exit: shutdown() can only
    // kill what is already stored, so if the exit latch flipped while the
    // tree was being created, kill the fresh tree HERE (on unix its process
    // group would otherwise outlive the shell).
    if plugins.exiting.load(Ordering::SeqCst) {
        let _ = child.graceful();
        child.force();
        plugins.busy.store(false, Ordering::SeqCst);
        return;
    }

    let mut tail: Vec<String> = Vec::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    let app_c = app.clone();
    let mut flush = move |lines: &mut Vec<(String, String)>| {
        if lines.is_empty() {
            return;
        }
        let payload: Vec<serde_json::Value> = lines
            .drain(..)
            .map(|(stream, line)| serde_json::json!({ "stream": stream, "line": line }))
            .collect();
        let _ = app_c.emit("plugin-log", serde_json::json!(payload));
    };
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    let child = {
        let mut child = child;
        for (stream, pipe) in [
            (
                "stdout",
                child
                    .child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            ),
            (
                "stderr",
                child
                    .child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            ),
        ] {
            if let Some(pipe) = pipe {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                        if tx.send((stream.to_string(), line)).is_err() {
                            break;
                        }
                    }
                });
            }
        }
        child
    };
    // The readers hold clones; the ORIGINAL sender must go before the loop
    // or `Disconnected` never fires (cancel takes the child out of the
    // runner, and without Disconnected the loop would spin forever — busy
    // stuck, plugin-done never emitted).
    drop(tx);
    *plugins.child.lock().unwrap_or_else(|p| p.into_inner()) = Some(child);
    // Second half of the exit race: shutdown() may have flipped the latch
    // between the post-spawn check above and this store — it would have
    // taken None, so reclaim and kill the tree ourselves.
    if plugins.exiting.load(Ordering::SeqCst) {
        if let Some(child) = plugins
            .child
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = child.graceful();
            child.force();
        }
        plugins.busy.store(false, Ordering::SeqCst);
        return;
    }
    fn handle_line(
        tail: &mut Vec<String>,
        pending: &mut Vec<(String, String)>,
        flush: &mut impl FnMut(&mut Vec<(String, String)>),
        stream: String,
        line: String,
    ) {
        tail.push(format!("[{stream}] {line}"));
        if tail.len() > 300 {
            tail.remove(0);
        }
        pending.push((stream, line));
        if pending.len() >= 64 {
            flush(pending);
        }
    }
    let exit = loop {
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok((stream, line)) => handle_line(&mut tail, &mut pending, &mut flush, stream, line),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => flush(&mut pending),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                flush(&mut pending);
                // Pipes closed: normally the tree has exited and wait()
                // recovers the REAL exit code (reporting null would make
                // the UI show "terminated" for successful installs). Take
                // the handle FIRST so the wait() does not hold the runner
                // mutex — cancel/app-exit must stay able to kill a hung
                // tree. If cancel took it already, a canceled op keeps its
                // null code.
                let handle = plugins
                    .child
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take();
                break handle
                    .and_then(|mut c| c.child.wait().ok())
                    .and_then(|s| s.code());
            }
        }
        if let Some(child) = plugins
            .child
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            if let Some(status) = child.child.try_wait().ok().flatten() {
                // Fast-exit path: drain what the reader threads already
                // queued (exit closes the pipes, so they wind down within
                // a tick) so the last log lines reach the UI/tail, then
                // report the code try_wait() reaped.
                while let Ok((stream, line)) = rx.recv_timeout(std::time::Duration::from_millis(50))
                {
                    handle_line(&mut tail, &mut pending, &mut flush, stream, line);
                }
                flush(&mut pending);
                break status.code();
            }
        }
    };
    *plugins.child.lock().unwrap_or_else(|p| p.into_inner()) = None;
    plugins.busy.store(false, Ordering::SeqCst);
    let _ = app.emit(
        "plugin-done",
        serde_json::json!({ "exit": exit, "tail": tail.join("\n") }),
    );
}

fn plugin_op(
    app: AppHandle,
    runtime: &Runtime,
    plugins: Arc<crate::plugins::PluginRunner>,
    name: String,
    op: &'static str,
) -> Result<(), String> {
    if !crate::plugins::is_valid_package_name(&name) {
        return Err(format!("invalid package name: {name:?}"));
    }
    if plugins.busy.swap(true, Ordering::SeqCst) {
        return Err("an operation is already running".to_string());
    }
    let Some(paths) = runtime.paths() else {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err("runtime paths are not resolved yet".to_string());
    };
    std::thread::spawn(move || {
        run_plugin_op(app, paths, plugins, name, op);
    });
    Ok(())
}

#[tauri::command]
pub fn install_plugin(
    app: AppHandle,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    name: String,
) -> Result<(), String> {
    plugin_op(app, &runtime, plugins.inner().clone(), name, "add")
}

#[tauri::command]
pub fn uninstall_plugin(
    app: AppHandle,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    name: String,
) -> Result<(), String> {
    plugin_op(app, &runtime, plugins.inner().clone(), name, "remove")
}

#[tauri::command]
pub fn cancel_plugin_op(plugins: State<'_, Arc<crate::plugins::PluginRunner>>) {
    let child = plugins
        .child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(mut child) = child {
        // Polite signal first. On Windows graceful() only works when the
        // shell initialized a hidden console (see main) — when it reports
        // false there is nothing to wait for, so escalate immediately.
        let polite = child.graceful();
        // Give the tree a moment, then finish the job — the same escalation
        // as the sidecar's shutdown path. Taking the handle also prevents
        // the done-path from racing the kill.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(if polite { 2 } else { 0 }));
            // If the tree already exited and the run thread reaped it, skip
            // the kill: the pgid may already have been recycled.
            if child.child.try_wait().ok().flatten().is_some() {
                return;
            }
            child.force();
        });
    }
}

// ---------------------------------------------------------------------------
// Deep-link confirmation hand-off: Rust parses and validates the URL before
// the UI ever sees it. `get_pending_plugin_install` drains the cold-start
// request after the webview mounts; `dismiss_pending_plugin_install` clears
// it when the user cancels (warm events leave the same slot populated, so
// cancellation must not leak into the next mount).
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_pending_plugin_install(
    pending: State<'_, crate::deep_link::PendingPluginInstall>,
) -> Option<crate::deep_link::PluginInstallRequest> {
    pending.take()
}

#[tauri::command]
pub fn dismiss_pending_plugin_install(
    pending: State<'_, crate::deep_link::PendingPluginInstall>,
    arbiter: State<'_, crate::deep_link::InstallArbiter>,
) {
    pending.clear();
    arbiter.release();
}

// ---------------------------------------------------------------------------
// Remote preset deep-link flow (dsharness://preset/install). The webview
// only ever passes a request_id; the validated download URL stays in Rust.
// ---------------------------------------------------------------------------

fn remote_preset_dir(dsh_home: &std::path::Path, request_id: &str) -> std::path::PathBuf {
    dsh_home
        .join(".desktop-tools")
        .join("preset-remote")
        .join(request_id)
}

fn remove_remote_preset_dir(dsh_home: &std::path::Path, request_id: &str) {
    let dir = remote_preset_dir(dsh_home, request_id);
    let _ = std::fs::remove_dir_all(dir);
}

/// Remove stale remote-preset download directories left by a previous run.
/// Only touches the preset-remote subtree; a symlink in its place is removed
/// as a link, never followed.
pub fn sweep_remote_preset_temp(runtime: &Runtime) {
    let Some(paths) = runtime.paths() else {
        return;
    };
    let root = paths.dsh_home.join(".desktop-tools").join("preset-remote");
    match std::fs::symlink_metadata(&root) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&root);
        }
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(&root);
        }
        _ => {}
    }
}

fn remote_preset_stage(session: &crate::deep_link::RemotePresetSession) -> &'static str {
    match session.state {
        crate::deep_link::RemotePresetState::AwaitingDownloadConsent => "awaiting-download",
        crate::deep_link::RemotePresetState::Downloading => "downloading",
        crate::deep_link::RemotePresetState::AwaitingInstallConsent { .. } => "awaiting-install",
    }
}

#[tauri::command]
pub fn get_pending_remote_preset(
    pending: State<'_, crate::deep_link::PendingRemotePreset>,
) -> Option<Value> {
    let session = pending.snapshot()?;
    Some(serde_json::json!({
        "requestId": session.request_id,
        "source": session.source,
        "stage": remote_preset_stage(&session),
    }))
}

#[tauri::command]
pub fn dismiss_remote_preset(
    pending: State<'_, crate::deep_link::PendingRemotePreset>,
    arbiter: State<'_, crate::deep_link::InstallArbiter>,
) {
    if let Some(archive) = pending.clear() {
        if let Some(dir) = archive.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    arbiter.release();
}

#[tauri::command]
pub async fn confirm_remote_preset_download(
    request_id: String,
    runtime: State<'_, Runtime>,
    pending: State<'_, crate::deep_link::PendingRemotePreset>,
) -> Result<Value, String> {
    use futures_util::StreamExt;

    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;
    let url = pending.begin_download(&request_id)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("client init failed: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        pending.clear();
        return Err(format!("download failed: HTTP {}", resp.status()));
    }
    let max = crate::deep_link::MAX_REMOTE_PRESET_BYTES as usize;
    if resp.content_length().is_some_and(|n| n > max as u64) {
        pending.clear();
        return Err("preset exceeds 16 MiB".to_string());
    }

    let dir = remote_preset_dir(std::path::Path::new(&dsh_home), &request_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create temp dir: {e}"))?;
    let archive = dir.join("archive.dshpreset");

    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            let _ = std::fs::remove_dir_all(&dir);
            format!("read failed: {e}")
        })?;
        if body.len().saturating_add(chunk.len()) > max {
            let _ = std::fs::remove_dir_all(&dir);
            return Err("preset exceeds 16 MiB".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    std::fs::write(&archive, &body).map_err(|e| {
        let _ = std::fs::remove_dir_all(&dir);
        format!("cannot write temp archive: {e}")
    })?;

    let preview = crate::preset::inspect_archive(&archive).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&dir);
    })?;
    pending.complete_download(&request_id, archive, preview.clone())?;

    Ok(serde_json::json!({
        "requestId": request_id,
        "id": preview.id,
        "files": preview.files,
        "warnings": preview.warnings,
    }))
}

#[tauri::command]
pub fn import_remote_preset(
    request_id: String,
    runtime: State<'_, Runtime>,
    pending: State<'_, crate::deep_link::PendingRemotePreset>,
    arbiter: State<'_, crate::deep_link::InstallArbiter>,
) -> Result<String, String> {
    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;
    let (archive, _preview) = pending.take_archive(&request_id)?;
    let result = crate::preset::install_archive(&archive, std::path::Path::new(&dsh_home));
    remove_remote_preset_dir(std::path::Path::new(&dsh_home), &request_id);
    arbiter.release();
    result
}
