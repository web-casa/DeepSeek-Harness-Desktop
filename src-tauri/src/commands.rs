//! IPC commands for the bootstrap window.
//!
//! Every command here is permission-gated by the app ACL (see build.rs):
//! only the local "bootstrap" window has the `allow-*` grants; the remote
//! Harness WebView has an empty capability set and cannot invoke anything.

use crate::diagnostics::redact;
use crate::harness::{
    child_alive, open_harness_window, publish_snapshot, request_restart, send_raw,
    snapshot_payload, Runtime, CMD_ID_SHUTDOWN,
};
use dsh_sidecar::platform::{PlatformChild, SpawnSpec};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

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
        // Clipboard diagnostics are frequently pasted into issue trackers.
        // Preserve whether DSH_HOME resolved without exposing its local path.
        "dshHome": s.dsh_home.as_ref().map(|_| "<DSH_HOME>"),
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
    if crate::build_info::STORE_BUILD {
        let _ = app;
        return Ok(serde_json::json!({ "available": false, "unsupported": true }));
    }
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
    if crate::build_info::STORE_BUILD {
        let _ = app;
        return Err("updates are managed by the Microsoft Store".to_string());
    }
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

// Diagnostic export lives in `diagnostics.rs`; this module only exposes the
// lightweight clipboard snapshot above and the application quit command.

/// Exit the whole desktop app (graceful shutdown runs in RunEvent::Exit).
#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ---------------------------------------------------------------------------
// Plugin-market commands (cordis.run). All network I/O happens here; the
// bootstrap webview is not permitted to fetch external hosts (CSP).
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn pick_sideload_file(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, mut rx) = tauri::async_runtime::channel::<Option<std::path::PathBuf>>(1);
    app.dialog()
        .file()
        .add_filter("dsh plugin package", &["tgz"])
        .pick_file(move |p| {
            let _ = tx.try_send(p.map(|f| f.into_path().unwrap_or_default()));
        });
    let path = rx
        .recv()
        .await
        .ok_or_else(|| "file dialog closed".to_string())?;
    Ok(path.map(|p| p.to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn market_search(
    query: String,
    category: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
    platform: Option<String>,
    runtime: State<'_, Runtime>,
    market: State<'_, std::sync::Arc<crate::market::MarketClient>>,
) -> Result<Value, String> {
    if platform.as_deref().is_some_and(|value| value != "desktop") {
        return Err("Desktop market requests must use platform=desktop".to_string());
    }
    let dsh_version = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .versions
        .harness
        .clone();
    market
        .search(
            &query,
            category.as_deref(),
            limit,
            cursor.as_deref(),
            &dsh_version,
        )
        .await
}

#[tauri::command]
pub async fn market_plugin(
    slug: String,
    runtime: State<'_, Runtime>,
    market: State<'_, std::sync::Arc<crate::market::MarketClient>>,
) -> Result<Value, String> {
    let dsh_version = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .versions
        .harness
        .clone();
    market.detail(&slug, &dsh_version).await
}

#[tauri::command]
pub async fn market_image(
    url: String,
    market: State<'_, std::sync::Arc<crate::market::MarketClient>>,
) -> Result<Value, String> {
    market.image(&url).await
}

/// Fetch an installable market entry again and expose the exact current
/// entryRevision to the confirmation dialog. This command never mutates a
/// profile; the following market_install_plugin call revalidates it once more
/// so a stale dialog cannot install a changed entry.
#[tauri::command]
pub async fn market_prepare_install(
    slug: String,
    runtime: State<'_, Runtime>,
    market: State<'_, std::sync::Arc<crate::market::MarketClient>>,
) -> Result<Value, String> {
    let dsh_version = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .versions
        .harness
        .clone();
    let candidate = market.prepare_install(&slug, &dsh_version).await?;
    serde_json::to_value(candidate)
        .map_err(|error| format!("cannot serialize market install preview: {error}"))
}

/// Begin the reviewed market installation lifecycle:
/// integrity metadata validation -> pre-disable -> pnpm with scripts disabled
/// -> local verification -> pending activation. The actual pnpm work runs in
/// the same cancellable process group/Job Object as normal plugin operations.
#[tauri::command]
pub async fn market_install_plugin(
    app: AppHandle,
    slug: String,
    entry_revision: String,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    market: State<'_, std::sync::Arc<crate::market::MarketClient>>,
) -> Result<(), String> {
    let plugins = plugins.inner().clone();
    if plugins.busy.swap(true, Ordering::SeqCst) {
        return Err("an operation is already running".to_string());
    }
    let dsh_version = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .versions
        .harness
        .clone();
    let candidate = match market.prepare_install(&slug, &dsh_version).await {
        Ok(candidate) => candidate,
        Err(error) => {
            plugins.busy.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    if crate::build_info::STORE_BUILD
        && !crate::curated_plugins::is_allowed(&candidate.package_name)
    {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err("仅允许安装 cordis.run 已审核插件".to_string());
    }
    if candidate.entry_revision != entry_revision {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err(
            "market entry changed; review the latest entryRevision before installing".to_string(),
        );
    }
    let Some(paths) = runtime.paths() else {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err("runtime paths are not resolved yet".to_string());
    };
    if let Err(error) = ensure_no_plugin_recovery(&runtime) {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err(error);
    }
    if let Err(error) = crate::plugins::pre_disable_market_plugin(&paths.dsh_home, &candidate) {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err(error);
    }
    std::thread::spawn(move || {
        run_market_pnpm(app, paths, plugins, candidate);
    });
    Ok(())
}

/// Activate a previously verified pending market package. It refetches and
/// checks the catalog entry first, so a newly blocked/deprecated/incompatible
/// package cannot be re-enabled by stale local pending state.
#[tauri::command]
pub async fn activate_market_plugin(
    slug: String,
    entry_revision: String,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    market: State<'_, std::sync::Arc<crate::market::MarketClient>>,
) -> Result<(), String> {
    let plugins = plugins.inner().clone();
    if plugins.busy.swap(true, Ordering::SeqCst) {
        return Err("an operation is already running".to_string());
    }
    let dsh_version = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .versions
        .harness
        .clone();
    let candidate = match market.prepare_install(&slug, &dsh_version).await {
        Ok(candidate) => candidate,
        Err(error) => {
            plugins.busy.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    if crate::build_info::STORE_BUILD
        && !crate::curated_plugins::is_allowed(&candidate.package_name)
    {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err("仅允许激活 cordis.run 已审核插件".to_string());
    }
    if candidate.entry_revision != entry_revision {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err(
            "market entry changed; install and review the latest revision before activation"
                .to_string(),
        );
    }
    if let Err(error) = ensure_no_plugin_recovery(&runtime) {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err(error);
    }
    let result = runtime
        .paths()
        .ok_or_else(|| "runtime paths are not resolved yet".to_string())
        .and_then(|paths| crate::plugins::activate_market_plugin(&paths.dsh_home, &candidate));
    plugins.busy.store(false, Ordering::SeqCst);
    result
}

// ---------------------------------------------------------------------------
// Journaled plugin startup recovery.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_plugin_recovery(runtime: State<'_, Runtime>) -> Result<Value, String> {
    let paths = runtime
        .paths()
        .ok_or_else(|| "runtime paths are not resolved yet".to_string())?;
    let (logs, terminal) = {
        let state = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state.logs.clone(), state.terminal_startup_failure)
    };
    let overview = crate::recovery::overview(&paths.dsh_home, &logs, terminal)?;
    serde_json::to_value(overview)
        .map_err(|error| format!("cannot serialize plugin recovery overview: {error}"))
}

#[tauri::command]
pub fn begin_plugin_recovery(
    app: AppHandle,
    package_name: String,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
) -> Result<Value, String> {
    let plugins = plugins.inner().clone();
    if plugins.busy.swap(true, Ordering::SeqCst) {
        return Err("an operation is already running".to_string());
    }
    let result = (|| {
        let paths = runtime
            .paths()
            .ok_or_else(|| "runtime paths are not resolved yet".to_string())?;
        let (logs, terminal) = {
            let state = runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (state.logs.clone(), state.terminal_startup_failure)
        };
        let transaction = crate::recovery::begin(&paths.dsh_home, &logs, terminal, &package_name)?;
        if let Some(observability) = app.try_state::<Arc<crate::observability::Observability>>() {
            observability.record(
                "plugin_recovery_pre_disabled",
                serde_json::json!({ "marketManaged": transaction.market_managed }),
            );
        }
        serde_json::to_value(transaction)
            .map_err(|error| format!("cannot serialize plugin recovery transaction: {error}"))
    })();
    plugins.busy.store(false, Ordering::SeqCst);
    let transaction = result?;
    request_restart(&app).map_err(|error| {
        format!("plugin was safely disabled, but Harness restart failed: {error}")
    })?;
    Ok(transaction)
}

#[tauri::command]
pub async fn rollback_plugin_recovery(
    app: AppHandle,
    transaction_id: String,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    market: State<'_, std::sync::Arc<crate::market::MarketClient>>,
) -> Result<(), String> {
    let plugins = plugins.inner().clone();
    if plugins.busy.swap(true, Ordering::SeqCst) {
        return Err("an operation is already running".to_string());
    }
    let paths = match runtime.paths() {
        Some(paths) => paths,
        None => {
            plugins.busy.store(false, Ordering::SeqCst);
            return Err("runtime paths are not resolved yet".to_string());
        }
    };
    let receipt = match crate::recovery::rollback_receipt(&paths.dsh_home, &transaction_id) {
        Ok(receipt) => receipt,
        Err(error) => {
            plugins.busy.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    if let Some(receipt) = receipt {
        let dsh_version = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .versions
            .harness
            .clone();
        let candidate = match market.prepare_install(&receipt.slug, &dsh_version).await {
            Ok(candidate) => candidate,
            Err(error) => {
                plugins.busy.store(false, Ordering::SeqCst);
                return Err(format!(
                    "market-managed plugin cannot be re-enabled without live approval: {error}"
                ));
            }
        };
        if !receipt.matches(&candidate) {
            plugins.busy.store(false, Ordering::SeqCst);
            return Err(
                "market entry changed; recovery rollback cannot re-enable the recorded package"
                    .to_string(),
            );
        }
        if let Err(error) =
            crate::plugins::verify_market_installation(&paths.dsh_home, &candidate, true)
        {
            plugins.busy.store(false, Ordering::SeqCst);
            return Err(format!(
                "market-managed plugin failed local integrity revalidation: {error}"
            ));
        }
    } else if crate::build_info::STORE_BUILD {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err(
            "Microsoft Store recovery cannot re-enable a plugin without a live market receipt"
                .to_string(),
        );
    }
    if let Err(error) = crate::recovery::rollback(&paths.dsh_home, &transaction_id) {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err(error);
    }
    if let Some(observability) = app.try_state::<Arc<crate::observability::Observability>>() {
        observability.record("plugin_recovery_rolled_back", serde_json::json!({}));
    }
    let restart_result = request_restart(&app);
    plugins.busy.store(false, Ordering::SeqCst);
    restart_result
}

#[tauri::command]
pub fn finalize_plugin_recovery(
    app: AppHandle,
    transaction_id: String,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
) -> Result<(), String> {
    let plugins = plugins.inner().clone();
    if plugins.busy.swap(true, Ordering::SeqCst) {
        return Err("an operation is already running".to_string());
    }
    let result = runtime
        .paths()
        .ok_or_else(|| "runtime paths are not resolved yet".to_string())
        .and_then(|paths| crate::recovery::finalize(&paths.dsh_home, &transaction_id));
    plugins.busy.store(false, Ordering::SeqCst);
    result?;
    if let Some(observability) = app.try_state::<Arc<crate::observability::Observability>>() {
        observability.record("plugin_recovery_finalized", serde_json::json!({}));
    }
    Ok(())
}

fn ensure_no_plugin_recovery(runtime: &Runtime) -> Result<(), String> {
    let Some(paths) = runtime.paths() else {
        return Err("runtime paths are not resolved yet".to_string());
    };
    if crate::recovery::has_active_transaction(&paths.dsh_home)? {
        return Err(
            "finish or roll back the active plugin recovery before another plugin mutation"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        is_zip_content_type, read_installed_plugins, redact, sweep_sideload_dir,
        sweep_sideloads_root,
    };

    #[test]
    fn sideload_sweep_does_not_follow_symlinked_tools_parent() {
        let root =
            std::env::temp_dir().join(format!("dsh-sideload-parent-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let outside_sideload = outside.join("sideload");
        std::fs::create_dir_all(&outside_sideload).unwrap();
        let victim = outside_sideload.join("victim.tgz");
        std::fs::write(&victim, b"x").unwrap();
        let link = root.join(".desktop-tools");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &link).unwrap();
        let referenced = std::collections::HashSet::new();
        sweep_sideloads_root(&link, &referenced);
        assert!(victim.is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sideload_sweep_does_not_follow_symlink_dir() {
        let root = std::env::temp_dir().join(format!("dsh-sideload-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("victim.tgz");
        std::fs::write(&victim, b"x").unwrap();
        let link = root.join("sideload-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &link).unwrap();
        let referenced = std::collections::HashSet::new();
        sweep_sideload_dir(&link, &referenced);
        assert!(victim.is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sideload_sweep_keeps_referenced_tarball() {
        let dir = std::env::temp_dir().join(format!("dsh-sideload-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keep = dir.join("keep.tgz");
        let stale = dir.join("stale.tgz");
        std::fs::write(&keep, b"a").unwrap();
        std::fs::write(&stale, b"b").unwrap();
        let referenced = std::collections::HashSet::from([keep.clone()]);
        sweep_sideload_dir(&dir, &referenced);
        assert!(keep.is_file());
        assert!(!stale.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
    fn remote_preset_requires_zip_content_type() {
        let zip = reqwest::header::HeaderValue::from_static("application/zip; charset=binary");
        let json = reqwest::header::HeaderValue::from_static("application/json");
        assert!(is_zip_content_type(Some(&zip)));
        assert!(!is_zip_content_type(Some(&json)));
        assert!(!is_zip_content_type(None));
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
    arbiter: State<'_, crate::deep_link::InstallArbiter>,
) -> Result<Value, String> {
    use tauri_plugin_dialog::DialogExt;
    if !arbiter.try_acquire(crate::deep_link::PendingInstallKind::LocalPresetPicker) {
        return Err("another install flow is active".to_string());
    }
    let (tx, rx) = std::sync::mpsc::channel::<Option<std::path::PathBuf>>();
    app.dialog()
        .file()
        .add_filter("dshpreset", &["dshpreset"])
        .pick_file(move |p| {
            let _ = tx.send(p.map(|f| f.into_path().unwrap_or_default()));
        });
    let path = match rx.recv().map_err(|e| e.to_string()) {
        Ok(Some(path)) => path,
        Ok(None) => {
            arbiter.release();
            return Err("cancelled".to_string());
        }
        Err(error) => {
            arbiter.release();
            return Err(error);
        }
    };
    let preview = match crate::preset::inspect_archive(&path) {
        Ok(preview) => preview,
        Err(error) => {
            arbiter.release();
            return Err(error);
        }
    };
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

/// Release the local-picker arbiter slot when the user closes the preview
/// without importing.
#[tauri::command]
pub fn cancel_preset_preview(
    pending: State<'_, PendingPreset>,
    arbiter: State<'_, crate::deep_link::InstallArbiter>,
) {
    *pending
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    arbiter.release();
}

/// Install the previously previewed archive (two-phase: the confirmation
/// dialog sits between preview_preset and this call).
#[tauri::command]
pub fn import_preset(
    runtime: State<'_, Runtime>,
    pending: State<'_, PendingPreset>,
    arbiter: State<'_, crate::deep_link::InstallArbiter>,
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
            arbiter.release();
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
#[cfg(test)]
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
        .map(|paths| crate::plugins::installed_plugins(&paths.dsh_home))
        .unwrap_or_default();
    serde_json::json!({
        "plugins": entries,
        // The backend busy flag survives webview reloads; the UI must be
        // able to resync instead of showing a stale idle state while an op
        // is still running (single-flight is app-wide).
        "busy": plugins.busy.load(Ordering::SeqCst),
    })
}

fn run_plugin_spec(
    app: AppHandle,
    paths: crate::paths::RuntimePaths,
    plugins: Arc<crate::plugins::PluginRunner>,
    plugin_spec: String,
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
            let _ = app.emit(
                "plugin-done",
                serde_json::json!({ "exit": 1, "tail": e, "op": op }),
            );
            plugins.busy.store(false, Ordering::SeqCst);
            return;
        }
    };
    let path_env = match std::env::join_paths(
        std::iter::once(shim_dir.as_os_str().to_owned())
            .chain(std::env::var_os("PATH").map(|old| old.to_owned())),
    ) {
        Ok(path_env) => path_env.to_string_lossy().to_string(),
        Err(e) => {
            let _ = app.emit(
                "plugin-done",
                serde_json::json!({ "exit": 1, "tail": format!("cannot build PATH: {e}"), "op": op }),
            );
            plugins.busy.store(false, Ordering::SeqCst);
            return;
        }
    };
    let spawn_spec = SpawnSpec {
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
            plugin_spec.clone(),
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
    let child = match PlatformChild::spawn(&spawn_spec, &inherited) {
        Ok(c) => c,
        Err(e) => {
            let _ = app.emit(
                "plugin-done",
                serde_json::json!({ "exit": 1, "tail": format!("spawn failed: {e}"), "op": op }),
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
    let mut exit = loop {
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
    if let Err(error) = crate::plugins::reconcile_active_market_receipts(&paths.dsh_home) {
        handle_line(
            &mut tail,
            &mut pending,
            &mut flush,
            "verify".to_string(),
            format!("plugin operation completed, but market provenance cleanup failed: {error}"),
        );
        flush(&mut pending);
        exit = Some(1);
    }
    plugins.busy.store(false, Ordering::SeqCst);
    let _ = app.emit(
        "plugin-done",
        serde_json::json!({ "exit": exit, "tail": tail.join("\n"), "op": op }),
    );
}

/// Run the market-only direct pnpm path. The official dsh plugin command
/// cannot be used here because it reconciles installed bundles into the
/// active profile automatically. This keeps the same PlatformChild process
/// tree and logging guarantees as the normal plugin operation, but runs only
/// pnpm add with lifecycle scripts disabled.
fn run_market_pnpm(
    app: AppHandle,
    paths: crate::paths::RuntimePaths,
    plugins: Arc<crate::plugins::PluginRunner>,
    candidate: crate::market::MarketInstallCandidate,
) {
    use std::io::{BufRead, BufReader};

    let profile = match crate::plugins::market_profile_dir(&paths.dsh_home) {
        Ok(profile) => profile,
        Err(error) => {
            let _ = app.emit(
                "plugin-done",
                serde_json::json!({ "exit": 1, "tail": error, "op": "market-install" }),
            );
            plugins.busy.store(false, Ordering::SeqCst);
            return;
        }
    };
    let pnpm = paths
        .harness_dir
        .join("node_modules")
        .join("pnpm")
        .join("bin")
        .join("pnpm.cjs");
    let spawn_spec = SpawnSpec {
        node: paths.node.to_string_lossy().to_string(),
        script: pnpm.to_string_lossy().to_string(),
        args: vec![
            "add".to_string(),
            candidate.tarball.clone(),
            "--ignore-scripts".to_string(),
            "--save-exact".to_string(),
            "--yes".to_string(),
            "--reporter=append-only".to_string(),
            format!("--registry={}", candidate.registry),
        ],
        cwd: profile.to_string_lossy().to_string(),
        env: vec![(
            "DSH_HOME".to_string(),
            paths.dsh_home.to_string_lossy().to_string(),
        )],
    };
    let inherited = std::env::vars_os().collect::<Vec<_>>();
    let child = match PlatformChild::spawn(&spawn_spec, &inherited) {
        Ok(child) => child,
        Err(error) => {
            let _ = app.emit(
                "plugin-done",
                serde_json::json!({ "exit": 1, "tail": format!("market pnpm spawn failed: {error}"), "op": "market-install" }),
            );
            plugins.busy.store(false, Ordering::SeqCst);
            return;
        }
    };
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
                    .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
            ),
            (
                "stderr",
                child
                    .child
                    .stderr
                    .take()
                    .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
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
    drop(tx);
    *plugins
        .child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(child);
    if plugins.exiting.load(Ordering::SeqCst) {
        if let Some(child) = plugins
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    let mut exit = loop {
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok((stream, line)) => handle_line(&mut tail, &mut pending, &mut flush, stream, line),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => flush(&mut pending),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                flush(&mut pending);
                let handle = plugins
                    .child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                break handle
                    .and_then(|mut child| child.child.wait().ok())
                    .and_then(|status| status.code());
            }
        }
        if let Some(child) = plugins
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            if let Some(status) = child.child.try_wait().ok().flatten() {
                while let Ok((stream, line)) = rx.recv_timeout(std::time::Duration::from_millis(50))
                {
                    handle_line(&mut tail, &mut pending, &mut flush, stream, line);
                }
                flush(&mut pending);
                break status.code();
            }
        }
    };
    *plugins
        .child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    if exit == Some(0) {
        if let Err(error) =
            crate::plugins::verify_and_mark_market_pending(&paths.dsh_home, &candidate)
        {
            handle_line(
                &mut tail,
                &mut pending,
                &mut flush,
                "verify".to_string(),
                error,
            );
            flush(&mut pending);
            exit = Some(1);
        }
    }
    plugins.busy.store(false, Ordering::SeqCst);
    let _ = app.emit(
        "plugin-done",
        serde_json::json!({ "exit": exit, "tail": tail.join("\n"), "op": "market-install" }),
    );
}

fn spawn_plugin_spec(
    app: AppHandle,
    runtime: &Runtime,
    plugins: Arc<crate::plugins::PluginRunner>,
    spec: String,
    op: &'static str,
) -> Result<(), String> {
    if plugins.busy.swap(true, Ordering::SeqCst) {
        return Err("an operation is already running".to_string());
    }
    if let Err(error) = ensure_no_plugin_recovery(runtime) {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err(error);
    }
    let Some(paths) = runtime.paths() else {
        plugins.busy.store(false, Ordering::SeqCst);
        return Err("runtime paths are not resolved yet".to_string());
    };
    std::thread::spawn(move || {
        run_plugin_spec(app, paths, plugins, spec, op);
    });
    Ok(())
}

fn plugin_op(
    app: AppHandle,
    runtime: &Runtime,
    plugins: Arc<crate::plugins::PluginRunner>,
    spec: String,
    op: &'static str,
) -> Result<(), String> {
    if !crate::plugins::is_valid_package_name(&spec) {
        return Err(format!("invalid package name: {spec:?}"));
    }
    spawn_plugin_spec(app, runtime, plugins, spec, op)
}

#[tauri::command]
pub fn install_plugin(
    app: AppHandle,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    name: String,
) -> Result<(), String> {
    if crate::build_info::STORE_BUILD && !crate::curated_plugins::is_allowed(&name) {
        return Err("仅允许安装 cordis.run 已审核插件".to_string());
    }
    plugin_op(app, &runtime, plugins.inner().clone(), name, "add")
}

#[tauri::command]
pub fn sideload_plugin(
    app: AppHandle,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    path: String,
) -> Result<(), String> {
    // A local archive cannot be tied to the reviewed immutable Store
    // allowlist, so Store builds must reject sideloading server-side even if
    // a compromised or stale bootstrap UI invokes this command directly.
    if crate::build_info::STORE_BUILD {
        return Err("Microsoft Store 版不支持离线侧载插件".to_string());
    }
    let src = std::path::Path::new(&path);
    let Some(paths) = runtime.paths() else {
        return Err("runtime paths are not resolved yet".to_string());
    };
    let staged = crate::plugins::stage_sideload(&paths.dsh_home, src)?;
    let spec = format!("file:{}", staged.display());
    #[cfg(windows)]
    if !crate::plugins::is_shell_safe_spec(&spec) {
        let _ = std::fs::remove_file(&staged);
        return Err(
            "当前用户目录路径包含空格或特殊字符，离线安装暂不可用；请使用插件市场在线安装"
                .to_string(),
        );
    }
    if let Err(error) = spawn_plugin_spec(app, &runtime, plugins.inner().clone(), spec, "add") {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    Ok(())
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

fn valid_request_id(request_id: &str) -> bool {
    request_id.len() == 32
        && request_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_zip_content_type(value: Option<&reqwest::header::HeaderValue>) -> bool {
    value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/zip"))
}

fn ensure_remote_tools_dir(dsh_home: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let tools = dsh_home.join(".desktop-tools");
    match std::fs::symlink_metadata(&tools) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err("refusing to use symlinked .desktop-tools".to_string())
        }
        Ok(meta) if !meta.is_dir() => return Err(".desktop-tools is not a directory".to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&tools).map_err(|e| format!("cannot create tools dir: {e}"))?;
        }
        Err(e) => return Err(format!("cannot inspect tools dir: {e}")),
    }
    let root = tools.join("preset-remote");
    match std::fs::symlink_metadata(&root) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err("refusing to use symlinked preset-remote".to_string())
        }
        Ok(meta) if !meta.is_dir() => return Err("preset-remote is not a directory".to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&root)
                .map_err(|e| format!("cannot create preset-remote dir: {e}"))?;
        }
        Err(e) => return Err(format!("cannot inspect preset-remote dir: {e}")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot chmod preset-remote dir: {e}"))?;
    }
    Ok(root)
}

fn create_remote_preset_dir(
    dsh_home: &std::path::Path,
    request_id: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    if !valid_request_id(request_id) {
        return Err("invalid request_id".to_string());
    }
    let root = ensure_remote_tools_dir(dsh_home)?;
    let dir = root.join(request_id);
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => return Err("request directory already exists".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect request dir: {e}")),
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create request dir: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot chmod request dir: {e}"))?;
    }
    Ok((dir.clone(), dir.join("archive.dshpreset")))
}

fn remove_remote_preset_dir(dsh_home: &std::path::Path, request_id: &str) {
    if !valid_request_id(request_id) {
        return;
    }
    let dir = remote_preset_dir(dsh_home, request_id);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&dir);
        }
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(&dir);
        }
        _ => {}
    }
}

/// Remove staged sideload tarballs that are no longer referenced by the web
/// profile. A successfully installed `file:` dependency must keep its source
/// tarball: pnpm re-reads that path for later lock/store operations.
pub fn sweep_stale_sideloads(runtime: &Runtime) {
    let Some(paths) = runtime.paths() else {
        return;
    };
    let referenced: std::collections::HashSet<std::path::PathBuf> = read_web_deps(&paths)
        .values()
        .filter_map(serde_json::Value::as_str)
        .filter_map(|spec| spec.strip_prefix("file:"))
        .map(std::path::PathBuf::from)
        .collect();
    sweep_sideloads_root(&paths.dsh_home.join(".desktop-tools"), &referenced);
}

fn sweep_sideloads_root(
    tools: &std::path::Path,
    referenced: &std::collections::HashSet<std::path::PathBuf>,
) {
    match std::fs::symlink_metadata(tools) {
        Ok(meta) if meta.file_type().is_symlink() => return,
        Ok(meta) if !meta.is_dir() => return,
        Ok(_) => {}
        Err(_) => return,
    }
    sweep_sideload_dir(&tools.join("sideload"), referenced);
}

fn sweep_sideload_dir(
    dir: &std::path::Path,
    referenced: &std::collections::HashSet<std::path::PathBuf>,
) {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let _ = std::fs::remove_file(dir);
            return;
        }
        Ok(meta) if !meta.is_dir() => return,
        Ok(_) => {}
        Err(_) => return,
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.to_ascii_lowercase().ends_with(".tgz") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if !referenced.contains(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn read_web_deps(paths: &crate::paths::RuntimePaths) -> serde_json::Map<String, serde_json::Value> {
    let path = paths
        .dsh_home
        .join("profiles")
        .join("web")
        .join("package.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return serde_json::Map::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return serde_json::Map::new();
    };
    json.get("dependencies")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn remote_preset_dir(dsh_home: &std::path::Path, request_id: &str) -> std::path::PathBuf {
    dsh_home
        .join(".desktop-tools")
        .join("preset-remote")
        .join(request_id)
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
        crate::deep_link::RemotePresetState::Installing { .. } => "installing",
    }
}

#[tauri::command]
pub fn get_pending_remote_preset(
    pending: State<'_, crate::deep_link::PendingRemotePreset>,
) -> Option<Value> {
    let session = pending.snapshot()?;
    let mut json = serde_json::json!({
        "requestId": session.request_id,
        "source": session.source,
        "stage": remote_preset_stage(&session),
    });
    if let crate::deep_link::RemotePresetState::AwaitingInstallConsent { preview, .. } =
        &session.state
    {
        json["id"] = serde_json::json!(preview.id);
        json["files"] = serde_json::json!(preview.files);
        json["warnings"] = serde_json::json!(preview.warnings);
    }
    Some(json)
}

#[tauri::command]
pub fn dismiss_remote_preset(
    request_id: String,
    pending: State<'_, crate::deep_link::PendingRemotePreset>,
    arbiter: State<'_, crate::deep_link::InstallArbiter>,
    runtime: State<'_, Runtime>,
) -> Result<(), String> {
    let removed = pending.dismiss(&request_id)?;
    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;
    if let Some(archive) = removed {
        if let Some(dir) = archive.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    remove_remote_preset_dir(std::path::Path::new(&dsh_home), &request_id);
    arbiter.release();
    Ok(())
}

#[tauri::command]
pub async fn confirm_remote_preset_download(
    request_id: String,
    runtime: State<'_, Runtime>,
    pending: State<'_, crate::deep_link::PendingRemotePreset>,
    arbiter: State<'_, crate::deep_link::InstallArbiter>,
) -> Result<Value, String> {
    use futures_util::StreamExt;

    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;

    // Every failure path below must converge through this closure: clear the
    // matching Downloading slot and release the global modal arbiter exactly
    // once. A stale worker must never clear a newer request.
    let fail = |pending: &crate::deep_link::PendingRemotePreset,
                arbiter: &crate::deep_link::InstallArbiter,
                request_id: &str,
                dir: Option<&std::path::Path>| {
        if pending.fail_download(request_id) {
            arbiter.release();
        }
        if let Some(dir) = dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    };

    let url = match pending.begin_download(&request_id) {
        Ok(url) => url,
        Err(error) => {
            // The slot may already be gone or owned by another request. If no
            // matching request is pending, release the arbiter so a stale
            // frontend retry cannot leave the modal permanently occupied.
            if pending.snapshot().is_none() {
                arbiter.release();
            }
            return Err(error);
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| {
            fail(&pending, &arbiter, &request_id, None);
            format!("client init failed: {e}")
        })?;
    let resp = client.get(&url).send().await.map_err(|e| {
        fail(&pending, &arbiter, &request_id, None);
        format!("download failed: {e}")
    })?;
    if resp.status() != reqwest::StatusCode::OK {
        fail(&pending, &arbiter, &request_id, None);
        return Err(format!("download failed: HTTP {}", resp.status()));
    }
    if !is_zip_content_type(resp.headers().get(reqwest::header::CONTENT_TYPE)) {
        fail(&pending, &arbiter, &request_id, None);
        return Err(
            "preset download must return application/zip directly from cordis.run".to_string(),
        );
    }
    let max = crate::deep_link::MAX_REMOTE_PRESET_BYTES as usize;
    if resp.content_length().is_some_and(|n| n > max as u64) {
        fail(&pending, &arbiter, &request_id, None);
        return Err("preset exceeds 16 MiB".to_string());
    }

    let (dir, archive) =
        match create_remote_preset_dir(std::path::Path::new(&dsh_home), &request_id) {
            Ok(v) => v,
            Err(e) => {
                fail(&pending, &arbiter, &request_id, None);
                return Err(e);
            }
        };

    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                fail(&pending, &arbiter, &request_id, Some(&dir));
                return Err(format!("read failed: {e}"));
            }
        };
        if body.len().saturating_add(chunk.len()) > max {
            fail(&pending, &arbiter, &request_id, Some(&dir));
            return Err("preset exceeds 16 MiB".to_string());
        }
        body.extend_from_slice(&chunk);
    }

    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive)
            .map_err(|e| {
                fail(&pending, &arbiter, &request_id, Some(&dir));
                format!("cannot write temp archive: {e}")
            })?;
        file.write_all(&body).map_err(|e| {
            fail(&pending, &arbiter, &request_id, Some(&dir));
            format!("cannot write temp archive: {e}")
        })?;
    }

    let preview = match crate::preset::inspect_archive(&archive) {
        Ok(preview) => preview,
        Err(error) => {
            fail(&pending, &arbiter, &request_id, Some(&dir));
            return Err(error);
        }
    };

    if let Err(error) = pending.complete_download(&request_id, archive, preview.clone()) {
        fail(&pending, &arbiter, &request_id, Some(&dir));
        return Err(error);
    }

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
    let archive = pending.begin_install(&request_id)?;
    match crate::preset::install_archive(&archive, std::path::Path::new(&dsh_home)) {
        Ok(id) => {
            pending.finish_install_success(&request_id);
            remove_remote_preset_dir(std::path::Path::new(&dsh_home), &request_id);
            arbiter.release();
            Ok(id)
        }
        Err(error) => {
            pending.finish_install_failure(&request_id);
            Err(error)
        }
    }
}
