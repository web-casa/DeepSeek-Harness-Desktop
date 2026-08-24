//! IPC commands for the bootstrap window.
//!
//! Every command here is permission-gated by the app ACL (see build.rs):
//! only the local "bootstrap" window has the `allow-*` grants; the remote
//! Harness WebView has an empty capability set and cannot invoke anything.

use crate::harness::{
    open_harness_window, request_restart, request_shutdown, snapshot_payload, Runtime, Status,
};
use crate::redaction::redact;
use dsh_sidecar::platform::{PlatformChild, SpawnSpec};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

const MAX_PLUGIN_LOG_LINE_BYTES: usize = 8 * 1024;
const PLUGIN_LOG_CHANNEL_CAPACITY: usize = 256;
const PLUGIN_EXIT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const PLUGIN_CANCEL_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// The manifest must use exact installer-specific keys (not a generic Windows
// key) so Tauri never falls back from an MSI installation to an NSIS payload.
// Keep these literals aligned with scripts/lib/release-artifacts.ts.
const WINDOWS_X64_NSIS_UPDATE_TARGET: &str = "windows-x86_64-nsis";
const WINDOWS_ARM64_NSIS_UPDATE_TARGET: &str = "windows-aarch64-nsis";
// The updater dependency has no default request deadline. Keep an offline
// network or a stalled GitHub connection from leaving the controller's update
// controls busy indefinitely. A download is deliberately more generous than
// a metadata check, but still bounded.
#[cfg(target_os = "windows")]
const UPDATE_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
#[cfg(target_os = "windows")]
const UPDATE_DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateSupport {
    InApp { target: &'static str },
    Unsupported { reason: &'static str },
}

fn update_support(
    store_build: bool,
    snap_runtime: bool,
    os: &str,
    arch: &str,
    bundle: Option<tauri::utils::config::BundleType>,
) -> UpdateSupport {
    if store_build {
        return UpdateSupport::Unsupported { reason: "store" };
    }
    if snap_runtime {
        // `snapd` owns the mounted revision and transactional rollback. Never
        // let the generic Tauri updater attempt to replace Snap-managed files.
        return UpdateSupport::Unsupported { reason: "snap" };
    }
    if os != "windows" {
        // macOS requires a notarized updater archive and Linux must defer to
        // its package manager / Flatpak remote. Neither is safe to fake as
        // an in-app installer today.
        return UpdateSupport::Unsupported { reason: "manual" };
    }
    if bundle != Some(tauri::utils::config::BundleType::Nsis) {
        return UpdateSupport::Unsupported {
            reason: match bundle {
                Some(tauri::utils::config::BundleType::Msi) => "msi",
                _ => "installer",
            },
        };
    }
    match arch {
        "x86_64" => UpdateSupport::InApp {
            target: WINDOWS_X64_NSIS_UPDATE_TARGET,
        },
        "aarch64" => UpdateSupport::InApp {
            target: WINDOWS_ARM64_NSIS_UPDATE_TARGET,
        },
        _ => UpdateSupport::Unsupported {
            reason: "architecture",
        },
    }
}

fn current_update_support() -> UpdateSupport {
    update_support(
        crate::build_info::STORE_BUILD,
        crate::build_info::is_snap_runtime(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        tauri::utils::platform::bundle_type(),
    )
}

fn unsupported_update_response(reason: &str) -> Value {
    serde_json::json!({ "available": false, "unsupported": true, "unsupportedReason": reason })
}

fn update_not_supported_error(reason: &str) -> String {
    match reason {
        "store" => "updates are managed by the Microsoft Store".to_string(),
        "snap" => "updates are managed by the Snap Store and snapd".to_string(),
        "msi" => "this MSI installation uses matching MSI or Store updates; install the next MSI manually".to_string(),
        "manual" => "this platform uses its native package manager or a manually downloaded installer".to_string(),
        "architecture" => "in-app updates are unavailable for this CPU architecture".to_string(),
        _ => "this installation type does not support in-app updates".to_string(),
    }
}

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
pub fn shutdown(app: AppHandle) -> Result<(), String> {
    request_shutdown(&app)
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
// Updater (Windows NSIS only for now).
// The update package's authenticity is enforced by the minisign pubkey
// embedded at build time (independent of app code signing). MSI, Store,
// macOS, and Linux deliberately use their own safe/manual update paths.
// ---------------------------------------------------------------------------

/// Result of a silent update check, surfaced to the bootstrap UI.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Value, String> {
    let target = match current_update_support() {
        UpdateSupport::InApp { target } => target,
        UpdateSupport::Unsupported { reason } => return Ok(unsupported_update_response(reason)),
    };
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, target);
        // This is unreachable for reviewed non-Windows builds, but protects
        // against a future policy change accidentally initializing the
        // updater on an unsupported platform.
        Ok(unsupported_update_response("manual"))
    }
    #[cfg(target_os = "windows")]
    {
        use tauri_plugin_updater::UpdaterExt;
        // An explicit target removes the plugin's generic fallback search:
        // `windows-x86_64-nsis` must never fall through to a generic key or
        // cross installer families.
        let updater = app
            .updater_builder()
            .target(target)
            .timeout(UPDATE_CHECK_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
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
    let target = match current_update_support() {
        UpdateSupport::InApp { target } => target,
        UpdateSupport::Unsupported { reason } => return Err(update_not_supported_error(reason)),
    };
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, target);
        Err(update_not_supported_error("manual"))
    }
    #[cfg(target_os = "windows")]
    {
        use tauri_plugin_updater::UpdaterExt;
        let plugins = app
            .state::<Arc<crate::plugins::PluginRunner>>()
            .inner()
            .clone();
        if !plugins.try_begin_update() {
            return Err("a plugin operation or another update is already running".to_string());
        }

        let result = async {
            let app_before_exit = app.clone();
            let updater = app
                .updater_builder()
                .target(target)
                .timeout(UPDATE_DOWNLOAD_TIMEOUT)
                // Tauri's default updater hook only clears its own window
                // resources, then Windows calls `std::process::exit(0)`.
                // Replace it so our sidecar, plugin trees, and lifecycle
                // evidence get the same orderly handoff as a normal quit.
                .on_before_exit(move || prepare_for_updater_exit(&app_before_exit))
                .build()
                .map_err(|e| e.to_string())?;
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
                .map_err(|e| format!("update install failed: {e}"))
        }
        .await;
        // Successful Windows handoff never returns: the updater launches the
        // verified installer and exits this process. Ordinary failures must
        // release the lease so plugin operations remain usable.
        plugins.finish_update();
        result
    }
}

#[cfg(target_os = "windows")]
fn prepare_for_updater_exit(app: &AppHandle) {
    // `download_and_install` invokes this immediately before ShellExecuteW
    // and `std::process::exit(0)`. It does not emit RunEvent::Exit, so all
    // shell-owned child trees must be handled here rather than relying on the
    // usual event-loop cleanup path.
    let observability = app
        .try_state::<Arc<crate::observability::Observability>>()
        .map(|state| state.inner().clone());
    if let Some(observability) = &observability {
        observability.record("desktop_update_handoff_started", serde_json::json!({}));
    }
    if let Some(plugins) = app.try_state::<Arc<crate::plugins::PluginRunner>>() {
        plugins.shutdown();
    }
    if let Some(runtime) = app.try_state::<Runtime>() {
        sweep_remote_preset_temp(&runtime);
        sweep_stale_sideloads(&runtime);
        crate::harness::shutdown_blocking(app);
    }
    if let Some(observability) = observability {
        observability.record("desktop_update_handoff_completed", serde_json::json!({}));
        if let Err(error) = observability.mark_clean() {
            eprintln!("failed to finalize Desktop update handoff evidence: {error}");
        }
    }
    // Preserve Tauri's default hook after our lifecycle handoff. The updater
    // exits immediately after this callback returns, so no Tauri API is used
    // beyond this point.
    app.cleanup_before_exit();
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
    begin_plugin_mutation(&plugins, &runtime)?;
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
            plugins.finish();
            return Err(error);
        }
    };
    if plugins.cancellation_requested() {
        plugins.finish();
        return Err("plugin installation was cancelled before profile mutation".to_string());
    }
    if !crate::market::distribution_allows_package(
        &candidate.package_name,
        crate::build_info::STORE_BUILD,
    ) {
        plugins.finish();
        return Err("仅允许安装 cordis.run 已审核插件".to_string());
    }
    if candidate.entry_revision != entry_revision {
        plugins.finish();
        return Err(
            "market entry changed; review the latest entryRevision before installing".to_string(),
        );
    }
    let Some(paths) = runtime.paths() else {
        plugins.finish();
        return Err("runtime paths are not resolved yet".to_string());
    };
    if let Err(error) = ensure_no_plugin_recovery(&runtime) {
        plugins.finish();
        return Err(error);
    }
    if let Err(error) = crate::plugins::ensure_market_install_config(&paths.dsh_home) {
        plugins.finish();
        return Err(error);
    }
    if let Err(error) = crate::plugins::pre_disable_market_plugin(&paths.dsh_home, &candidate) {
        plugins.finish();
        return Err(error);
    }
    if plugins.cancellation_requested() {
        plugins.finish();
        return Err(
            "plugin installation was cancelled after safe pre-disable; the plugin remains disabled"
                .to_string(),
        );
    }
    let worker_plugins = plugins.clone();
    std::thread::Builder::new()
        .name("market-plugin-install".to_string())
        .spawn(move || {
            run_market_pnpm(app, paths, worker_plugins, candidate);
        })
        .map(|_| ())
        .map_err(|error| {
            plugins.finish();
            format!(
                "cannot start market plugin worker after safe pre-disable; the plugin remains disabled: {error}"
            )
        })
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
    begin_plugin_mutation(&plugins, &runtime)?;
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
            plugins.finish();
            return Err(error);
        }
    };
    if plugins.cancellation_requested() {
        plugins.finish();
        return Err("plugin activation was cancelled before profile mutation".to_string());
    }
    if !crate::market::distribution_allows_package(
        &candidate.package_name,
        crate::build_info::STORE_BUILD,
    ) {
        plugins.finish();
        return Err("仅允许激活 cordis.run 已审核插件".to_string());
    }
    if candidate.entry_revision != entry_revision {
        plugins.finish();
        return Err(
            "market entry changed; install and review the latest revision before activation"
                .to_string(),
        );
    }
    if let Err(error) = ensure_no_plugin_recovery(&runtime) {
        plugins.finish();
        return Err(error);
    }
    let result = runtime
        .paths()
        .ok_or_else(|| "runtime paths are not resolved yet".to_string())
        .and_then(|paths| crate::plugins::activate_market_plugin(&paths.dsh_home, &candidate));
    plugins.finish();
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
    begin_plugin_mutation(&plugins, &runtime)?;
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
    plugins.finish();
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
    begin_plugin_mutation(&plugins, &runtime)?;
    let paths = match runtime.paths() {
        Some(paths) => paths,
        None => {
            plugins.finish();
            return Err("runtime paths are not resolved yet".to_string());
        }
    };
    let receipt = match crate::recovery::rollback_receipt(&paths.dsh_home, &transaction_id) {
        Ok(receipt) => receipt,
        Err(error) => {
            plugins.finish();
            return Err(error);
        }
    };
    let approved_market_candidate = if let Some(receipt) = receipt {
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
                plugins.finish();
                return Err(format!(
                    "market-managed plugin cannot be re-enabled without live approval: {error}"
                ));
            }
        };
        if plugins.cancellation_requested() {
            plugins.finish();
            return Err("plugin recovery rollback was cancelled before re-enable".to_string());
        }
        if !receipt.matches(&candidate) {
            plugins.finish();
            return Err(
                "market entry changed; recovery rollback cannot re-enable the recorded package"
                    .to_string(),
            );
        }
        if !crate::market::distribution_allows_package(
            &candidate.package_name,
            crate::build_info::STORE_BUILD,
        ) {
            plugins.finish();
            return Err(
                "Microsoft Store recovery cannot re-enable a plugin removed from the reviewed snapshot"
                    .to_string(),
            );
        }
        if let Err(error) =
            crate::plugins::verify_market_installation(&paths.dsh_home, &candidate, true)
        {
            plugins.finish();
            return Err(format!(
                "market-managed plugin failed local integrity revalidation: {error}"
            ));
        }
        Some(candidate)
    } else if crate::build_info::STORE_BUILD {
        plugins.finish();
        return Err(
            "Microsoft Store recovery cannot re-enable a plugin without a live market receipt"
                .to_string(),
        );
    } else {
        None
    };
    if plugins.cancellation_requested() {
        plugins.finish();
        return Err("plugin recovery rollback was cancelled before profile mutation".to_string());
    }
    if let Some(candidate) = &approved_market_candidate {
        if let Err(error) = crate::plugins::record_active_market_receipt(&paths.dsh_home, candidate)
        {
            plugins.finish();
            return Err(format!(
                "cannot preserve reviewed market provenance before recovery rollback: {error}"
            ));
        }
    }
    if let Err(error) = crate::recovery::rollback(&paths.dsh_home, &transaction_id) {
        if let Some(candidate) = &approved_market_candidate {
            let _ = crate::plugins::remove_active_market_receipt(
                &paths.dsh_home,
                &candidate.package_name,
            );
        }
        plugins.finish();
        return Err(error);
    }
    if let Some(observability) = app.try_state::<Arc<crate::observability::Observability>>() {
        observability.record("plugin_recovery_rolled_back", serde_json::json!({}));
    }
    plugins.finish();
    request_restart(&app)
}

#[tauri::command]
pub fn finalize_plugin_recovery(
    app: AppHandle,
    transaction_id: String,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
) -> Result<(), String> {
    let plugins = plugins.inner().clone();
    begin_plugin_mutation(&plugins, &runtime)?;
    let result = runtime
        .paths()
        .ok_or_else(|| "runtime paths are not resolved yet".to_string())
        .and_then(|paths| crate::recovery::finalize(&paths.dsh_home, &transaction_id));
    plugins.finish();
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
    ensure_no_plugin_recovery_at(&paths.dsh_home)
}

/// Every mutation of the Web profile must preserve an in-flight recovery
/// journal's exact before/disabled bytes. Keep this backend-owned instead of
/// relying on the controller to hide a button: a stale or compromised
/// bootstrap webview can still invoke any command in its capability set.
fn ensure_no_plugin_recovery_at(dsh_home: &std::path::Path) -> Result<(), String> {
    if crate::recovery::has_active_transaction(dsh_home)? {
        return Err(
            "finish or roll back the active plugin recovery before another plugin mutation"
                .to_string(),
        );
    }
    Ok(())
}

fn plugin_mutation_status_allowed(status: Status) -> bool {
    !matches!(status, Status::Idle | Status::Starting | Status::Stopping)
}

/// Claim the plugin single-flight gate, then verify the Harness is not inside
/// a start/stop boundary. User and automatic restarts use the same gate, so
/// once this check succeeds no new restart can race pnpm/profile mutation.
fn begin_plugin_mutation(
    plugins: &crate::plugins::PluginRunner,
    runtime: &Runtime,
) -> Result<(), String> {
    if !plugins.try_begin() {
        return Err("an operation is already running".to_string());
    }
    let status = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .status;
    if !plugin_mutation_status_allowed(status) {
        plugins.finish();
        return Err("Harness 尚未就绪或正在启动/停止；状态稳定后才能修改插件".to_string());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        ensure_no_plugin_recovery_at, is_zip_content_type, manual_plugin_install_allowed,
        market_pnpm_args, parse_pnpm_major, plugin_mutation_status_allowed, plugin_path_env,
        redact, remove_pnpm_args, sweep_sideload_dir, sweep_sideloads_root,
        sweep_stale_sideloads_paths, update_support, UpdateSupport,
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
    fn store_builds_reject_the_generic_install_path() {
        assert!(manual_plugin_install_allowed(false));
        assert!(!manual_plugin_install_allowed(true));
    }

    #[test]
    fn updater_policy_is_exact_to_windows_nsis_and_cpu_architecture() {
        use tauri::utils::config::BundleType;

        assert_eq!(
            update_support(false, false, "windows", "x86_64", Some(BundleType::Nsis)),
            UpdateSupport::InApp {
                target: "windows-x86_64-nsis"
            }
        );
        assert_eq!(
            update_support(false, false, "windows", "aarch64", Some(BundleType::Nsis)),
            UpdateSupport::InApp {
                target: "windows-aarch64-nsis"
            }
        );
        assert_eq!(
            update_support(false, false, "windows", "x86_64", Some(BundleType::Msi)),
            UpdateSupport::Unsupported { reason: "msi" }
        );
        assert_eq!(
            update_support(false, false, "windows", "i686", Some(BundleType::Nsis)),
            UpdateSupport::Unsupported {
                reason: "architecture"
            }
        );
        assert_eq!(
            update_support(false, false, "linux", "x86_64", Some(BundleType::AppImage)),
            UpdateSupport::Unsupported { reason: "manual" }
        );
        assert_eq!(
            update_support(true, false, "windows", "x86_64", Some(BundleType::Nsis)),
            UpdateSupport::Unsupported { reason: "store" }
        );
        // Store policy stays stronger than the runtime environment marker;
        // a Store build must never be described as a Snap build.
        assert_eq!(
            update_support(true, true, "linux", "x86_64", Some(BundleType::AppImage)),
            UpdateSupport::Unsupported { reason: "store" }
        );
        assert_eq!(
            update_support(false, true, "linux", "x86_64", Some(BundleType::AppImage)),
            UpdateSupport::Unsupported { reason: "snap" }
        );
    }

    #[test]
    fn profile_patch_cleanup_gate_preserves_an_active_plugin_recovery() {
        let home = std::env::temp_dir().join(format!(
            "dsh-profile-cleanup-recovery-gate-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let profile = home.join("profiles/web");
        std::fs::create_dir_all(profile.join("node_modules/broken-plugin")).unwrap();
        std::fs::write(
            profile.join("package.json"),
            br#"{"dependencies":{"broken-plugin":"1.0.0"},"dsh":{"profile":{"bundles":["broken-plugin"]}}}"#,
        )
        .unwrap();
        std::fs::write(
            profile.join("node_modules/broken-plugin/package.json"),
            br#"{"name":"broken-plugin","version":"1.0.0","dependencies":{}}"#,
        )
        .unwrap();
        let transaction = crate::recovery::begin(
            &home,
            &[(
                "stderr".to_string(),
                "Error: failed at /tmp/node_modules/broken-plugin/index.js".to_string(),
            )],
            true,
            "broken-plugin",
        )
        .unwrap();

        assert!(ensure_no_plugin_recovery_at(&home)
            .unwrap_err()
            .contains("active plugin recovery"));

        crate::recovery::rollback(&home, &transaction.transaction_id).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn plugin_mutations_wait_for_harness_transition_boundaries() {
        assert!(!plugin_mutation_status_allowed(
            crate::harness::Status::Idle
        ));
        assert!(!plugin_mutation_status_allowed(
            crate::harness::Status::Starting
        ));
        assert!(!plugin_mutation_status_allowed(
            crate::harness::Status::Stopping
        ));
        assert!(plugin_mutation_status_allowed(
            crate::harness::Status::Running
        ));
        assert!(plugin_mutation_status_allowed(
            crate::harness::Status::Crashed
        ));
        assert!(plugin_mutation_status_allowed(
            crate::harness::Status::Stopped
        ));
    }

    #[test]
    fn market_pnpm_isolated_install_contract_is_complete() {
        let candidate = crate::market::MarketInstallCandidate {
            slug: "fixture-plugin".to_string(),
            entry_revision: "revision-1".to_string(),
            package_name: "fixture-plugin".to_string(),
            version: "1.0.0".to_string(),
            integrity: "sha512-fixture".to_string(),
            registry: "https://registry.npmjs.org".to_string(),
            tarball: "https://registry.npmjs.org/fixture-plugin/-/fixture-plugin-1.0.0.tgz"
                .to_string(),
        };
        let store = std::path::Path::new("private-market-store");
        let config = std::path::Path::new("private-market-config");
        let args = market_pnpm_args(&candidate, Some(store), config);

        assert_eq!(args[0], "add");
        assert_eq!(args[1], candidate.tarball);
        for required in [
            "--ignore-scripts",
            "--ignore-workspace",
            "--global=false",
            "--node-linker=hoisted",
            "--config.auto-install-peers=false",
            "--package-import-method=copy",
            "--virtual-store-dir=node_modules/.pnpm",
            "--config.enable-global-virtual-store=false",
            "--verify-store-integrity",
            "--config.strict-store-pkg-content-check=true",
            "--config.ignore-pnpmfile=true",
            "--save-exact",
            "--reporter=append-only",
            "--registry=https://registry.npmjs.org",
        ] {
            assert!(args.iter().any(|arg| arg == required), "missing {required}");
        }
        for required in [
            format!("--store-dir={}", store.display()),
            format!("--config.config-dir={}", config.display()),
            format!("--config.userconfig={}", config.join(".npmrc").display()),
            format!("--config.globalconfig={}", config.join(".npmrc").display()),
        ] {
            assert!(args.contains(&required), "missing {required}");
        }
    }

    #[test]
    fn direct_uninstall_never_uses_upstream_global_reconciliation() {
        let store = std::path::Path::new("existing-profile-store");
        let config = std::path::Path::new("private-plugin-config");
        let args = remove_pnpm_args("fixture-plugin", Some(store), config);
        assert_eq!(&args[..2], ["remove", "fixture-plugin"]);
        assert!(args.iter().any(|arg| arg == "--config.ignore-scripts=true"));
        assert!(args
            .iter()
            .any(|arg| arg == "--config.ignore-pnpmfile=true"));
        assert!(args.iter().any(|arg| arg == "--ignore-workspace"));
        assert!(args
            .iter()
            .any(|arg| arg == "--store-dir=existing-profile-store"));
        assert!(!args.iter().any(|arg| arg == "plugin"));
    }

    #[test]
    fn bundled_pnpm_major_is_strictly_parsed() {
        assert_eq!(parse_pnpm_major(br#"{"version":"11.21.0"}"#).unwrap(), 11);
        assert!(parse_pnpm_major(br#"{"version":"latest"}"#).is_err());
        assert!(parse_pnpm_major(br#"{"version":"11.beta"}"#).is_err());
        assert!(parse_pnpm_major(br#"{"name":"pnpm"}"#).is_err());
    }

    #[test]
    fn sideload_sweep_fails_closed_when_the_profile_is_unreadable() {
        let root = std::env::temp_dir().join(format!(
            "dsh-sideload-unreadable-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let profile = root.join("profiles/web");
        let sideload = root.join(".desktop-tools/sideload");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(&sideload).unwrap();
        std::fs::write(profile.join("package.json"), "not json").unwrap();
        let retained = sideload.join("retained.tgz");
        std::fs::write(&retained, b"archive").unwrap();
        let paths = crate::paths::RuntimePaths {
            sidecar: root.join("sidecar"),
            node: root.join("node"),
            harness_dir: root.join("harness"),
            dsh_home: root.clone(),
        };

        sweep_stale_sideloads_paths(&paths);
        assert!(retained.is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_path_prepends_shim_to_a_multi_segment_parent_path() {
        let parent = std::env::join_paths([
            std::path::Path::new("parent-one"),
            std::path::Path::new("parent-two"),
        ])
        .unwrap();
        let actual = plugin_path_env(std::path::Path::new("desktop-shim"), Some(&parent)).unwrap();
        assert_eq!(
            std::env::split_paths(&actual).collect::<Vec<_>>(),
            vec![
                std::path::PathBuf::from("desktop-shim"),
                std::path::PathBuf::from("parent-one"),
                std::path::PathBuf::from("parent-two"),
            ]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn plugin_path_preserves_empty_parent_segments_verbatim() {
        let inherited = std::ffi::OsStr::new("parent-one::parent-two:");
        let actual =
            plugin_path_env(std::path::Path::new("desktop-shim"), Some(inherited)).unwrap();
        assert_eq!(actual, "desktop-shim:parent-one::parent-two:");
    }

    #[test]
    fn plugin_path_without_parent_keeps_the_owned_shim() {
        let actual = plugin_path_env(std::path::Path::new("desktop-shim"), None).unwrap();
        assert_eq!(
            std::env::split_paths(&actual).collect::<Vec<_>>(),
            vec![std::path::PathBuf::from("desktop-shim")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn closed_log_stream_does_not_make_a_running_plugin_uncancellable() {
        use dsh_sidecar::platform::{PlatformChild, SpawnSpec};
        use std::sync::Arc;

        let runner = Arc::new(crate::plugins::PluginRunner::new());
        assert!(runner.try_begin());
        let child = PlatformChild::spawn(
            &SpawnSpec {
                node: "/bin/sh".to_string(),
                script: "-c".to_string(),
                args: vec!["exec 1>&- 2>&-; sleep 30".to_string()],
                cwd: std::env::temp_dir().to_string_lossy().to_string(),
                env: Vec::new(),
            },
            &std::env::vars_os().collect::<Vec<_>>(),
        )
        .unwrap();
        *runner.child.lock().unwrap() = Some(child);
        let (_tx, rx) = std::sync::mpsc::sync_channel(1);
        drop(_tx);

        let supervisor_runner = runner.clone();
        let supervisor = std::thread::spawn(move || {
            super::supervise_plugin_output(&supervisor_runner, rx, |_| {})
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            runner.child.lock().unwrap().is_some(),
            "pipe closure must not discard the live process handle"
        );
        let (accepted, child) = runner.request_cancel();
        assert!(accepted);
        let child = child.expect("cancel must recover the registered process tree");
        child.force();
        drop(child);
        runner.child_termination_finished();
        assert_eq!(supervisor.join().unwrap(), None);
        runner.finish();
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
            let _ = arbiter.release(crate::deep_link::PendingInstallKind::LocalPresetPicker);
            return Err("cancelled".to_string());
        }
        Err(error) => {
            let _ = arbiter.release(crate::deep_link::PendingInstallKind::LocalPresetPicker);
            return Err(error);
        }
    };
    let preview = match crate::preset::inspect_archive(&path) {
        Ok(preview) => preview,
        Err(error) => {
            let _ = arbiter.release(crate::deep_link::PendingInstallKind::LocalPresetPicker);
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
    let _ = arbiter.release(crate::deep_link::PendingInstallKind::LocalPresetPicker);
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
            let _ = arbiter.release(crate::deep_link::PendingInstallKind::LocalPresetPicker);
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
// Plugin installation (bundled pnpm + official `dsh plugin` add CLI) and
// precise direct-pnpm removal. The whole node → dsh/pnpm → node-gyp tree runs
// under dsh-sidecar's PlatformChild (process group / Job Object), so cancel
// and app exit clean it fully.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_plugins(
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
) -> Value {
    let entries = runtime
        .paths()
        .map(|paths| crate::plugins::installed_plugins(&paths.dsh_home))
        .unwrap_or_default();
    // This report is intentionally read-only.  It contains only bounded
    // package names and stable issue codes, never profile YAML or local paths.
    let consistency = runtime
        .paths()
        .map(|paths| crate::profile_consistency::report(&paths.dsh_home))
        .unwrap_or_default();
    serde_json::json!({
        "plugins": entries,
        "consistency": consistency,
        // The backend busy flag survives webview reloads; the UI must be
        // able to resync instead of showing a stale idle state while an op
        // is still running (single-flight is app-wide).
        "busy": plugins.is_busy(),
    })
}

/// Preview a profile-patch cleanup without mutating the user profile.  The
/// PluginRunner boundary serializes this snapshot against all Desktop-owned
/// plugin/profile mutations; apply performs an additional byte-for-byte
/// recheck in case Harness or the user changed the file in the meantime.
#[tauri::command]
pub fn preview_profile_patch_cleanup(
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    cleanup: State<'_, crate::profile_consistency::PendingProfileCleanup>,
) -> Result<crate::profile_consistency::ProfileCleanupPreview, String> {
    let paths = runtime
        .paths()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;
    plugins.with_idle_profile(|| {
        ensure_no_plugin_recovery_at(&paths.dsh_home)?;
        crate::profile_consistency::preview_cleanup(&paths.dsh_home, &cleanup)
    })
}

/// Commit one volatile, user-confirmed profile-patch cleanup preview.  The
/// opaque transaction id is not an authority by itself: the backend also
/// requires the same DSH_HOME and unchanged original patch bytes.
#[tauri::command]
pub fn apply_profile_patch_cleanup(
    transaction_id: String,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    cleanup: State<'_, crate::profile_consistency::PendingProfileCleanup>,
) -> Result<crate::profile_consistency::ProfileCleanupPreview, String> {
    let paths = runtime
        .paths()
        .ok_or_else(|| "DSH_HOME is unknown".to_string())?;
    plugins.with_idle_profile(|| {
        ensure_no_plugin_recovery_at(&paths.dsh_home)?;
        crate::profile_consistency::apply_cleanup(&paths.dsh_home, &transaction_id, &cleanup)
    })
}

/// Prepend the Desktop-owned pnpm shim without parsing and rebuilding the
/// inherited PATH. `join_paths` validates only the app-owned segment; feeding
/// the serialized parent value to it as one segment caused the reported
/// separator error and rebuilding can rewrite Windows quoting/empty segments.
fn plugin_path_env(
    shim_dir: &std::path::Path,
    inherited_path: Option<&std::ffi::OsStr>,
) -> Result<std::ffi::OsString, std::env::JoinPathsError> {
    let mut path = std::env::join_paths([shim_dir.as_os_str()])?;
    if let Some(inherited_path) = inherited_path.filter(|path| !path.is_empty()) {
        #[cfg(windows)]
        path.push(";");
        #[cfg(not(windows))]
        path.push(":");
        path.push(inherited_path);
    }
    Ok(path)
}

/// Upstream invokes pnpm through `shell: true` on Windows. Do not let an
/// inherited, attacker-controlled ComSpec redirect that reviewed operation to
/// an arbitrary executable: resolve cmd.exe from the OS system directory and
/// pair it with the standard executable-extension search contract.
#[cfg(windows)]
fn trusted_windows_comspec() -> Result<String, String> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    // Windows' documented extended path limit is 32,767 UTF-16 code units;
    // the system directory is normally far shorter, but a fixed upper bound
    // avoids trusting an inherited environment variable or allocating from an
    // untrusted API length.
    let mut buffer = vec![0_u16; 32_768];
    // SAFETY: buffer is writable for the advertised length and remains alive
    // for the duration of the synchronous Win32 call.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 {
        return Err(format!(
            "cannot resolve the trusted Windows system directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    if length >= buffer.len() {
        return Err(
            "trusted Windows system directory exceeds the supported path limit".to_string(),
        );
    }
    let mut path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
    path.push("cmd.exe");
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("cannot inspect trusted Windows command shell: {error}"))?;
    if crate::secure_fs::is_symlink_or_reparse(&metadata) || !metadata.is_file() {
        return Err("trusted Windows command shell must be a regular file".to_string());
    }
    path.into_os_string()
        .into_string()
        .map_err(|_| "trusted Windows command shell path is not valid Unicode".to_string())
}

enum PluginChildPoll {
    Running,
    Missing,
    Exited(Option<i32>, PlatformChild),
    Failed(String, PlatformChild),
}

fn poll_plugin_child(plugins: &crate::plugins::PluginRunner) -> PluginChildPoll {
    let mut slot = plugins
        .child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(child) = slot.as_mut() else {
        return PluginChildPoll::Missing;
    };
    let status = child.child.try_wait();
    match status {
        Ok(None) => PluginChildPoll::Running,
        Ok(Some(status)) => match slot.take() {
            Some(child) => PluginChildPoll::Exited(status.code(), child),
            None => PluginChildPoll::Missing,
        },
        Err(error) => match slot.take() {
            Some(child) => PluginChildPoll::Failed(error.to_string(), child),
            None => PluginChildPoll::Missing,
        },
    }
}

fn attach_plugin_log_readers(
    child: &mut PlatformChild,
    tx: &std::sync::mpsc::SyncSender<(String, String)>,
) -> Result<(), String> {
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
        let Some(pipe) = pipe else {
            continue;
        };
        let tx = tx.clone();
        std::thread::Builder::new()
            .name(format!("plugin-{stream}-reader"))
            .spawn(move || {
                let _ = dsh_sidecar::for_each_bounded_line(
                    std::io::BufReader::new(pipe),
                    MAX_PLUGIN_LOG_LINE_BYTES,
                    |line| tx.send((stream.to_string(), line)).is_ok(),
                );
            })
            .map_err(|error| format!("cannot start plugin {stream} reader: {error}"))?;
    }
    Ok(())
}

/// Plugin stdout can contain arbitrary package-manager output and must stay
/// session-only. An explicit detailed-diagnostics choice permits bounded,
/// redacted stderr evidence for a failed installation or removal instead.
fn record_plugin_stderr(
    app: &AppHandle,
    paths: &crate::paths::RuntimePaths,
    stream: &str,
    line: &str,
) {
    if stream != "stderr" {
        return;
    }
    if let Some(mode) = app.try_state::<crate::diagnostic_mode::DiagnosticMode>() {
        mode.record_line(
            crate::diagnostic_mode::DetailedLogSource::PluginStderr,
            line,
            &paths.dsh_home.to_string_lossy(),
        );
    }
}

/// Drive one registered plugin process without using pipe closure as a proxy
/// for process exit. A child can close stdout/stderr and keep running; keeping
/// its handle registered lets Cancel and app-exit still terminate that tree.
/// The receiver is bounded, so a noisy pnpm/plugin cannot move an unbounded
/// amount of output from the OS pipe into Desktop heap memory.
fn supervise_plugin_output(
    plugins: &crate::plugins::PluginRunner,
    rx: std::sync::mpsc::Receiver<(String, String)>,
    mut on_event: impl FnMut(Option<(String, String)>),
) -> Option<i32> {
    let mut readers_disconnected = false;
    let mut child_missing_since: Option<std::time::Instant> = None;

    loop {
        if readers_disconnected {
            std::thread::sleep(std::time::Duration::from_millis(50));
        } else {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(line) => on_event(Some(line)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => on_event(None),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    readers_disconnected = true;
                    on_event(None);
                }
            }
        }

        match poll_plugin_child(plugins) {
            PluginChildPoll::Running => child_missing_since = None,
            PluginChildPoll::Missing => {
                let since = child_missing_since.get_or_insert_with(std::time::Instant::now);
                if readers_disconnected || since.elapsed() >= PLUGIN_CANCEL_DRAIN_TIMEOUT {
                    on_event(None);
                    return None;
                }
            }
            PluginChildPoll::Exited(code, child) => {
                // PlatformChild::drop tears down any descendants that kept
                // the inherited pipes open after the direct pnpm/dsh process
                // exited. Drop outside the runner mutex so Cancel never waits
                // behind output draining.
                drop(child);
                drain_plugin_output(&rx, &mut on_event);
                return code;
            }
            PluginChildPoll::Failed(error, child) => {
                on_event(Some((
                    "supervisor".to_string(),
                    format!("cannot poll plugin process: {error}"),
                )));
                drop(child);
                drain_plugin_output(&rx, &mut on_event);
                return Some(1);
            }
        }
    }
}

fn drain_plugin_output(
    rx: &std::sync::mpsc::Receiver<(String, String)>,
    on_event: &mut impl FnMut(Option<(String, String)>),
) {
    let deadline = std::time::Instant::now() + PLUGIN_EXIT_DRAIN_TIMEOUT;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok(line) => on_event(Some(line)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                if std::time::Instant::now() >= deadline =>
            {
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    on_event(None);
}

fn emit_plugin_done(
    app: &AppHandle,
    paths: &crate::paths::RuntimePaths,
    plugins: &crate::plugins::PluginRunner,
    exit: Option<i32>,
    tail: String,
    op: &'static str,
) {
    sweep_stale_sideloads_paths(paths);
    plugins.finish_with(|| {
        let _ = app.emit(
            "plugin-done",
            serde_json::json!({ "exit": exit, "tail": tail, "op": op }),
        );
    });
}

fn parse_pnpm_major(package_json: &[u8]) -> Result<u64, String> {
    let package: serde_json::Value = serde_json::from_slice(package_json)
        .map_err(|error| format!("bundled pnpm package.json is invalid JSON: {error}"))?;
    let version = package
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "bundled pnpm package.json has no version".to_string())?;
    semver::Version::parse(version)
        .map(|version| version.major)
        .map_err(|error| format!("bundled pnpm version is invalid: {error}"))
}

fn bundled_pnpm_major(paths: &crate::paths::RuntimePaths) -> Result<u64, String> {
    let manifest = paths
        .harness_dir
        .join("node_modules")
        .join("pnpm")
        .join("package.json");
    let bytes = crate::secure_fs::read_bounded(&manifest, 256 * 1024)?
        .ok_or_else(|| "bundled pnpm package.json is missing".to_string())?;
    parse_pnpm_major(&bytes)
}

fn prepare_plugin_pnpm_config(dsh_home: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let tools = crate::plugins::market_tools_dir(dsh_home)?;
    let config_home = tools.join("pnpm-config");
    crate::secure_fs::ensure_private_dir(&config_home)
        .and_then(|()| crate::secure_fs::atomic_write(&config_home.join(".npmrc"), b"", 1024))
        .map_err(|error| format!("cannot prepare isolated plugin pnpm configuration: {error}"))?;
    Ok(config_home)
}

fn isolated_pnpm_args(store_dir: Option<&std::path::Path>) -> Vec<String> {
    let mut args = vec![
        "--ignore-workspace".to_string(),
        "--global=false".to_string(),
        "--node-linker=hoisted".to_string(),
        "--config.auto-install-peers=false".to_string(),
        "--package-import-method=copy".to_string(),
        "--virtual-store-dir=node_modules/.pnpm".to_string(),
        "--yes".to_string(),
        "--reporter=append-only".to_string(),
    ];
    if let Some(store_dir) = store_dir {
        args.push(format!("--store-dir={}", store_dir.display()));
    }
    args
}

fn market_pnpm_args(
    candidate: &crate::market::MarketInstallCandidate,
    store_dir: Option<&std::path::Path>,
    config_home: &std::path::Path,
) -> Vec<String> {
    let mut args = vec!["add".to_string(), candidate.tarball.clone()];
    args.extend(isolated_pnpm_args(store_dir));
    let npmrc = config_home.join(".npmrc");
    args.extend([
        "--ignore-scripts".to_string(),
        "--config.ignore-pnpmfile=true".to_string(),
        "--config.enable-global-virtual-store=false".to_string(),
        "--verify-store-integrity".to_string(),
        "--config.strict-store-pkg-content-check=true".to_string(),
        format!("--config.config-dir={}", config_home.display()),
        format!("--config.userconfig={}", npmrc.display()),
        format!("--config.globalconfig={}", npmrc.display()),
    ]);
    args.push("--save-exact".to_string());
    args.push(format!("--registry={}", candidate.registry));
    args
}

fn remove_pnpm_args(
    package_name: &str,
    store_dir: Option<&std::path::Path>,
    config_home: &std::path::Path,
) -> Vec<String> {
    let mut args = vec!["remove".to_string(), package_name.to_string()];
    args.extend(isolated_pnpm_args(store_dir));
    let npmrc = config_home.join(".npmrc");
    // pnpm 11's remove command intentionally does not expose these install
    // settings as first-class flags. The documented `--config.*` escape hatch
    // applies them without the CLI rejecting the operation as unknown.
    args.extend([
        "--config.ignore-scripts=true".to_string(),
        "--config.ignore-pnpmfile=true".to_string(),
        "--config.enable-global-virtual-store=false".to_string(),
        "--config.verify-store-integrity=true".to_string(),
        "--config.strict-store-pkg-content-check=true".to_string(),
        format!("--config.config-dir={}", config_home.display()),
        format!("--config.userconfig={}", npmrc.display()),
        format!("--config.globalconfig={}", npmrc.display()),
    ]);
    args
}

fn plugin_spawn_spec(
    paths: &crate::paths::RuntimePaths,
    plugin_spec: &str,
    op: &'static str,
) -> Result<SpawnSpec, String> {
    let pnpm_cjs = paths
        .harness_dir
        .join("node_modules")
        .join("pnpm")
        .join("bin")
        .join("pnpm.cjs");
    if op == "remove" {
        let profile = crate::plugins::market_profile_dir(&paths.dsh_home)?;
        let config_home = prepare_plugin_pnpm_config(&paths.dsh_home)?;
        let store_dir = crate::plugins::pnpm_store_base(&profile, bundled_pnpm_major(paths)?)?;
        return Ok(SpawnSpec {
            node: paths.node.to_string_lossy().to_string(),
            script: pnpm_cjs.to_string_lossy().to_string(),
            args: remove_pnpm_args(plugin_spec, store_dir.as_deref(), &config_home),
            cwd: profile.to_string_lossy().to_string(),
            env: vec![
                (
                    "DSH_HOME".to_string(),
                    paths.dsh_home.to_string_lossy().to_string(),
                ),
                (
                    "XDG_CONFIG_HOME".to_string(),
                    config_home.to_string_lossy().to_string(),
                ),
                (
                    "NPM_CONFIG_USERCONFIG".to_string(),
                    config_home.join(".npmrc").to_string_lossy().to_string(),
                ),
            ],
        });
    }

    let shim_dir = crate::plugins::ensure_pnpm_shim(&paths.dsh_home, &paths.node, &pnpm_cjs)?;
    let inherited_path = std::env::var_os("PATH");
    let path_env = plugin_path_env(&shim_dir, inherited_path.as_deref())
        .map_err(|error| format!("cannot build PATH: {error}"))?;
    let store_dir =
        crate::plugins::generic_profile_store_base(&paths.dsh_home, bundled_pnpm_major(paths)?)?;
    let mut env = vec![
        (
            "DSH_HOME".to_string(),
            paths.dsh_home.to_string_lossy().to_string(),
        ),
        ("PATH".to_string(), path_env.to_string_lossy().to_string()),
    ];
    if let Some(store_dir) = store_dir {
        // PlatformChild removes inherited pnpm_config_* keys, then applies
        // these Desktop-owned overrides. Reusing only pnpm's recorded store
        // avoids both config injection and ERR_PNPM_UNEXPECTED_STORE.
        env.push((
            "PNPM_CONFIG_STORE_DIR".to_string(),
            store_dir.to_string_lossy().to_string(),
        ));
    }
    #[cfg(windows)]
    {
        env.push(("ComSpec".to_string(), trusted_windows_comspec()?));
        env.push(("PATHEXT".to_string(), ".COM;.EXE;.BAT;.CMD".to_string()));
    }
    Ok(SpawnSpec {
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
            plugin_spec.to_string(),
        ],
        cwd: paths.harness_dir.to_string_lossy().to_string(),
        env,
    })
}

fn run_plugin_spec(
    app: AppHandle,
    paths: crate::paths::RuntimePaths,
    plugins: Arc<crate::plugins::PluginRunner>,
    plugin_spec: String,
    op: &'static str,
) {
    if plugins.cancellation_requested() {
        emit_plugin_done(
            &app,
            &paths,
            &plugins,
            None,
            "plugin operation cancelled before start".to_string(),
            op,
        );
        return;
    }

    let spawn_spec = match plugin_spawn_spec(&paths, &plugin_spec, op) {
        Ok(spawn_spec) => spawn_spec,
        Err(mut error) => {
            if op == "remove" {
                error.push_str("; the requested plugin remains safely disabled");
            }
            emit_plugin_done(&app, &paths, &plugins, Some(1), error, op);
            return;
        }
    };
    if plugins.cancellation_requested() {
        emit_plugin_done(
            &app,
            &paths,
            &plugins,
            None,
            "plugin operation cancelled before spawn".to_string(),
            op,
        );
        return;
    }
    let inherited = std::env::vars_os().collect::<Vec<_>>();
    let child = match PlatformChild::spawn(&spawn_spec, &inherited) {
        Ok(c) => c,
        Err(e) => {
            let suffix = if op == "remove" {
                "; the requested plugin remains safely disabled"
            } else {
                ""
            };
            emit_plugin_done(
                &app,
                &paths,
                &plugins,
                Some(1),
                format!("spawn failed: {e}{suffix}"),
                op,
            );
            return;
        }
    };
    // Close the spawn/store race against RunEvent::Exit: shutdown() can only
    // kill what is already stored, so if the exit latch flipped while the
    // tree was being created, kill the fresh tree HERE (on unix its process
    // group would otherwise outlive the shell).
    if plugins.exiting.load(Ordering::SeqCst) || plugins.cancellation_requested() {
        let _ = child.graceful();
        child.force();
        if !plugins.exiting.load(Ordering::SeqCst) {
            emit_plugin_done(
                &app,
                &paths,
                &plugins,
                None,
                "plugin operation cancelled during spawn".to_string(),
                op,
            );
        } else {
            plugins.finish();
        }
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
    let (tx, rx) = std::sync::mpsc::sync_channel::<(String, String)>(PLUGIN_LOG_CHANNEL_CAPACITY);
    let mut child = child;
    if let Err(error) = attach_plugin_log_readers(&mut child, &tx) {
        drop(tx);
        child.force();
        drop(child);
        emit_plugin_done(&app, &paths, &plugins, Some(1), error, op);
        return;
    }
    // The readers hold clones; the ORIGINAL sender must go before the loop
    // or `Disconnected` never fires (cancel takes the child out of the
    // runner, and without Disconnected the loop would spin forever — busy
    // stuck, plugin-done never emitted).
    drop(tx);
    *plugins.child.lock().unwrap_or_else(|p| p.into_inner()) = Some(child);
    // Second half of the exit race: shutdown() may have flipped the latch
    // between the post-spawn check above and this store — it would have
    // taken None, so reclaim and kill the tree ourselves.
    if plugins.exiting.load(Ordering::SeqCst) || plugins.cancellation_requested() {
        if let Some(child) = plugins
            .child
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = child.graceful();
            child.force();
        }
        if !plugins.exiting.load(Ordering::SeqCst) {
            emit_plugin_done(
                &app,
                &paths,
                &plugins,
                None,
                "plugin operation cancelled before execution".to_string(),
                op,
            );
        } else {
            plugins.finish();
        }
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
    let mut exit = supervise_plugin_output(&plugins, rx, |event| match event {
        Some((stream, line)) => {
            record_plugin_stderr(&app, &paths, &stream, &line);
            handle_line(&mut tail, &mut pending, &mut flush, stream, line);
        }
        None => flush(&mut pending),
    });
    if let Err(error) = crate::plugins::reconcile_market_receipts(&paths.dsh_home) {
        handle_line(
            &mut tail,
            &mut pending,
            &mut flush,
            "verify".to_string(),
            format!("plugin operation completed, but market provenance cleanup failed: {error}"),
        );
        flush(&mut pending);
        // A successful direct removal is not undone by a corrupt optional
        // provenance receipt. Keep uninstall available as the fail-safe path;
        // malformed receipts grant no activation/recovery authority and the
        // warning remains visible in the operation log.
        if op != "remove" {
            exit = Some(1);
        }
    }
    emit_plugin_done(&app, &paths, &plugins, exit, tail.join("\n"), op);
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
    if plugins.cancellation_requested() {
        emit_plugin_done(
            &app,
            &paths,
            &plugins,
            None,
            "market install cancelled before start".to_string(),
            "market-install",
        );
        return;
    }

    let profile = match crate::plugins::market_profile_dir(&paths.dsh_home) {
        Ok(profile) => profile,
        Err(error) => {
            emit_plugin_done(&app, &paths, &plugins, Some(1), error, "market-install");
            return;
        }
    };
    if let Err(error) = crate::plugins::ensure_market_install_config(&paths.dsh_home) {
        emit_plugin_done(&app, &paths, &plugins, Some(1), error, "market-install");
        return;
    }
    let pnpm = paths
        .harness_dir
        .join("node_modules")
        .join("pnpm")
        .join("bin")
        .join("pnpm.cjs");
    let config_home = match prepare_plugin_pnpm_config(&paths.dsh_home) {
        Ok(config_home) => config_home,
        Err(error) => {
            emit_plugin_done(&app, &paths, &plugins, Some(1), error, "market-install");
            return;
        }
    };
    let store_dir = match bundled_pnpm_major(&paths)
        .and_then(|major| crate::plugins::pnpm_store_base(&profile, major))
    {
        Ok(store_dir) => store_dir,
        Err(error) => {
            emit_plugin_done(&app, &paths, &plugins, Some(1), error, "market-install");
            return;
        }
    };
    let spawn_spec = SpawnSpec {
        node: paths.node.to_string_lossy().to_string(),
        script: pnpm.to_string_lossy().to_string(),
        args: market_pnpm_args(&candidate, store_dir.as_deref(), &config_home),
        cwd: profile.to_string_lossy().to_string(),
        env: vec![
            (
                "DSH_HOME".to_string(),
                paths.dsh_home.to_string_lossy().to_string(),
            ),
            (
                "XDG_CONFIG_HOME".to_string(),
                config_home.to_string_lossy().to_string(),
            ),
            (
                "NPM_CONFIG_USERCONFIG".to_string(),
                config_home.join(".npmrc").to_string_lossy().to_string(),
            ),
        ],
    };
    let inherited = std::env::vars_os().collect::<Vec<_>>();
    if plugins.cancellation_requested() {
        emit_plugin_done(
            &app,
            &paths,
            &plugins,
            None,
            "market install cancelled before spawn".to_string(),
            "market-install",
        );
        return;
    }
    let child = match PlatformChild::spawn(&spawn_spec, &inherited) {
        Ok(child) => child,
        Err(error) => {
            emit_plugin_done(
                &app,
                &paths,
                &plugins,
                Some(1),
                format!("market pnpm spawn failed: {error}"),
                "market-install",
            );
            return;
        }
    };
    if plugins.exiting.load(Ordering::SeqCst) || plugins.cancellation_requested() {
        let _ = child.graceful();
        child.force();
        if !plugins.exiting.load(Ordering::SeqCst) {
            emit_plugin_done(
                &app,
                &paths,
                &plugins,
                None,
                "market install cancelled during spawn".to_string(),
                "market-install",
            );
        } else {
            plugins.finish();
        }
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
    let (tx, rx) = std::sync::mpsc::sync_channel::<(String, String)>(PLUGIN_LOG_CHANNEL_CAPACITY);
    let mut child = child;
    if let Err(error) = attach_plugin_log_readers(&mut child, &tx) {
        drop(tx);
        child.force();
        drop(child);
        emit_plugin_done(&app, &paths, &plugins, Some(1), error, "market-install");
        return;
    }
    drop(tx);
    *plugins
        .child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(child);
    if plugins.exiting.load(Ordering::SeqCst) || plugins.cancellation_requested() {
        if let Some(child) = plugins
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = child.graceful();
            child.force();
        }
        if !plugins.exiting.load(Ordering::SeqCst) {
            emit_plugin_done(
                &app,
                &paths,
                &plugins,
                None,
                "market install cancelled before execution".to_string(),
                "market-install",
            );
        } else {
            plugins.finish();
        }
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
    let mut exit = supervise_plugin_output(&plugins, rx, |event| match event {
        Some((stream, line)) => {
            record_plugin_stderr(&app, &paths, &stream, &line);
            handle_line(&mut tail, &mut pending, &mut flush, stream, line);
        }
        None => flush(&mut pending),
    });
    if exit == Some(0) && !plugins.cancellation_requested() {
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
    } else if exit == Some(0) {
        handle_line(
            &mut tail,
            &mut pending,
            &mut flush,
            "verify".to_string(),
            "market install completed, but activation remained disabled because cancellation was requested"
                .to_string(),
        );
        flush(&mut pending);
        exit = None;
    }
    emit_plugin_done(
        &app,
        &paths,
        &plugins,
        exit,
        tail.join("\n"),
        "market-install",
    );
}

fn spawn_plugin_worker(
    app: AppHandle,
    paths: crate::paths::RuntimePaths,
    plugins: Arc<crate::plugins::PluginRunner>,
    spec: String,
    op: &'static str,
) -> Result<(), String> {
    let worker_plugins = plugins.clone();
    std::thread::Builder::new()
        .name(format!("plugin-{op}"))
        .spawn(move || {
            run_plugin_spec(app, paths, worker_plugins, spec, op);
        })
        .map(|_| ())
        .map_err(|error| {
            plugins.finish();
            format!("cannot start plugin operation worker: {error}")
        })
}

fn spawn_plugin_spec(
    app: AppHandle,
    runtime: &Runtime,
    plugins: Arc<crate::plugins::PluginRunner>,
    spec: String,
    op: &'static str,
) -> Result<(), String> {
    begin_plugin_mutation(&plugins, runtime)?;
    if let Err(error) = ensure_no_plugin_recovery(runtime) {
        plugins.finish();
        return Err(error);
    }
    let Some(paths) = runtime.paths() else {
        plugins.finish();
        return Err("runtime paths are not resolved yet".to_string());
    };
    if let Err(error) = crate::plugins::ensure_no_pending_market_plugins(&paths.dsh_home) {
        plugins.finish();
        return Err(format!(
            "cannot start a generic plugin addition safely: {error}"
        ));
    }
    spawn_plugin_worker(app, paths, plugins, spec, op)
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

fn manual_plugin_install_allowed(store_build: bool) -> bool {
    !store_build
}

#[tauri::command]
pub fn install_plugin(
    app: AppHandle,
    runtime: State<'_, Runtime>,
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
    name: String,
) -> Result<(), String> {
    // Store installs must always cross the live market candidate gate. A
    // local allowlist alone cannot prove current blocked/deprecated state,
    // exact version/integrity, scripts-disabled installation, or pending
    // activation. Keep this generic npm-name command for website builds only.
    if !manual_plugin_install_allowed(crate::build_info::STORE_BUILD) {
        return Err("Microsoft Store 版只能通过 cordis.run 插件市场安装并显式激活插件".to_string());
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
    if !crate::plugins::is_valid_package_name(&name) {
        return Err(format!("invalid package name: {name:?}"));
    }
    let plugins = plugins.inner().clone();
    begin_plugin_mutation(&plugins, &runtime)?;
    if let Err(error) = ensure_no_plugin_recovery(&runtime) {
        plugins.finish();
        return Err(error);
    }
    let Some(paths) = runtime.paths() else {
        plugins.finish();
        return Err("runtime paths are not resolved yet".to_string());
    };
    if plugins.cancellation_requested() {
        plugins.finish();
        return Err("plugin uninstall was cancelled before profile mutation".to_string());
    }
    // Repair any valid pending receipt left active by an older build before
    // touching the requested package. Corrupt optional receipt state must not
    // block the one operation users need to remove an unsafe plugin.
    let _ = crate::plugins::reconcile_market_receipts(&paths.dsh_home);
    if let Err(error) = crate::plugins::pre_disable_installed_plugin(&paths.dsh_home, &name) {
        plugins.finish();
        return Err(error);
    }
    spawn_plugin_worker(app, paths, plugins, name, "remove")
        .map_err(|error| format!("{error}; the requested plugin remains safely disabled"))
}

#[tauri::command]
pub fn cancel_plugin_op(
    plugins: State<'_, Arc<crate::plugins::PluginRunner>>,
) -> Result<(), String> {
    // Latch first: the operation may still be in async market preparation or
    // between spawn and child registration. The worker checks this before and
    // after registering its handle, closing the old cancel-before-store race.
    let plugins = plugins.inner().clone();
    let (accepted, child) = plugins.request_cancel();
    if !accepted {
        return Ok(());
    }
    if let Some(mut child) = child {
        // Polite signal first. On Windows graceful() only works when the
        // shell initialized a hidden console (see main) — when it reports
        // false there is nothing to wait for, so escalate immediately.
        let polite = child.graceful();
        // Give the tree a moment, then finish the job — the same escalation
        // as the sidecar's shutdown path. Taking the handle also prevents
        // the done-path from racing the kill.
        let termination_plugins = plugins.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("plugin-cancel".to_string())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(if polite { 2 } else { 0 }));
                if child.child.try_wait().ok().flatten().is_none() {
                    child.force();
                }
                // PlatformChild::drop also tears down descendants after the
                // direct child exits (Job Object / process group guarantee).
                drop(child);
                termination_plugins.child_termination_finished();
            })
        {
            // The failed Builder drops the captured PlatformChild, whose Drop
            // tears down the whole tree. Re-open the gate only after that
            // synchronous drop has completed.
            plugins.child_termination_finished();
            return Err(format!("cannot start plugin cancellation worker: {error}"));
        }
    }
    Ok(())
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
    let _ = arbiter.release(crate::deep_link::PendingInstallKind::Plugin);
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
    let tools = crate::plugins::market_tools_dir(dsh_home)?;
    let root = tools.join("preset-remote");
    crate::secure_fs::ensure_private_dir(&root)
        .map_err(|e| format!("cannot prepare preset-remote dir: {e}"))?;
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
    std::fs::create_dir(&dir).map_err(|e| format!("cannot create request dir: {e}"))?;
    crate::secure_fs::ensure_private_dir(&dir)
        .map_err(|e| format!("cannot protect request dir: {e}"))?;
    Ok((dir.clone(), dir.join("archive.dshpreset")))
}

fn remove_reparse_leaf(path: &std::path::Path, metadata: &std::fs::Metadata) {
    #[cfg(windows)]
    {
        if metadata.is_dir() {
            let _ = std::fs::remove_dir(path);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        let _ = std::fs::remove_file(path);
    }
}

fn remove_remote_preset_dir(dsh_home: &std::path::Path, request_id: &str) {
    if !valid_request_id(request_id) {
        return;
    }
    let dir = remote_preset_dir(dsh_home, request_id);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if crate::secure_fs::is_symlink_or_reparse(&meta) => {
            remove_reparse_leaf(&dir, &meta);
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
    sweep_stale_sideloads_paths(&paths);
}

fn sweep_stale_sideloads_paths(paths: &crate::paths::RuntimePaths) {
    // A missing/corrupt/unreadable profile is uncertainty, not proof that no
    // sideload is referenced. Fail closed so a transient profile read during
    // startup, shutdown, or a killed pnpm write cannot destroy the retained
    // archive needed by an installed `file:` dependency.
    let Some(dependencies) = read_web_deps(paths) else {
        return;
    };
    let referenced: std::collections::HashSet<std::path::PathBuf> = dependencies
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
        Ok(meta) if crate::secure_fs::is_symlink_or_reparse(&meta) => return,
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
        Ok(meta) if crate::secure_fs::is_symlink_or_reparse(&meta) => return,
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
        if crate::secure_fs::is_symlink_or_reparse(&meta) {
            continue;
        }
        if !referenced.contains(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn read_web_deps(
    paths: &crate::paths::RuntimePaths,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let (_, json) = crate::plugins::read_profile_manifest(&paths.dsh_home).ok()?;
    json.get("dependencies")
        .and_then(serde_json::Value::as_object)
        .cloned()
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
        Ok(meta) if crate::secure_fs::is_symlink_or_reparse(&meta) => {
            remove_reparse_leaf(&root, &meta);
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
    // A failed or partially initialized Runtime must not make a user-visible
    // cancel permanently occupy the global modal slot. The archive path, when
    // present, is enough to remove its private request directory; the
    // DSH_HOME-based cleanup below is therefore best effort.
    let dsh_home = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dsh_home
        .clone();
    let removed = pending.dismiss(&request_id)?;
    if let Some(archive) = removed {
        if let Some(dir) = archive.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    if let Some(dsh_home) = dsh_home {
        remove_remote_preset_dir(std::path::Path::new(&dsh_home), &request_id);
    } else {
        eprintln!("[preset] DSH_HOME unavailable while dismissing remote preset; request data cleanup deferred");
    }
    let _ = arbiter.release(crate::deep_link::PendingInstallKind::RemotePreset);
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
            let _ = arbiter.release(crate::deep_link::PendingInstallKind::RemotePreset);
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
                let _ = arbiter.release(crate::deep_link::PendingInstallKind::RemotePreset);
            }
            return Err(error);
        }
    };

    let client = crate::tls::client_builder()
        .map_err(|error| {
            fail(&pending, &arbiter, &request_id, None);
            format!("client init failed: {error}")
        })?
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
        let mut file = crate::secure_fs::create_private_new(&archive).map_err(|e| {
            fail(&pending, &arbiter, &request_id, Some(&dir));
            format!("cannot write temp archive: {e}")
        })?;
        file.write_all(&body).map_err(|e| {
            fail(&pending, &arbiter, &request_id, Some(&dir));
            format!("cannot write temp archive: {e}")
        })?;
        file.sync_all().map_err(|e| {
            fail(&pending, &arbiter, &request_id, Some(&dir));
            format!("cannot sync temp archive: {e}")
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
            let _ = arbiter.release(crate::deep_link::PendingInstallKind::RemotePreset);
            Ok(id)
        }
        Err(error) => {
            pending.finish_install_failure(&request_id);
            Err(error)
        }
    }
}
