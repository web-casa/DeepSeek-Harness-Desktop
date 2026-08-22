//! Explicit, bounded detailed diagnostic capture for the trusted controller.
//!
//! Normal Desktop operation deliberately persists lifecycle facts only.  When
//! a user explicitly enables this mode, the controller additionally keeps a
//! small, redacted local record of stderr and Desktop-owned failure messages.
//! It never records Harness stdout, sessions, prompts, workspace files, or
//! uploads anything; detailed evidence enters an archive only when the user
//! separately chooses Export diagnostics.

use crate::observability::EvidenceFile;
use crate::secure_fs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, TryLockError};
use tauri::{AppHandle, Manager, State};

const PREFERENCE_SCHEMA: u8 = 1;
const PREFERENCE_MAX_BYTES: usize = 128;
const DETAIL_LOG_BYTES: u64 = 1024 * 1024;
const DETAIL_LOG_ROTATIONS: usize = 2;
const DETAIL_LINE_BYTES: usize = 8 * 1024;
const DETAIL_LOG_CHANNEL_CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetailedLogSource {
    HarnessStderr,
    SidecarStderr,
    PluginStderr,
    DesktopError,
}

impl DetailedLogSource {
    const fn name(self) -> &'static str {
        match self {
            Self::HarnessStderr => "harness-stderr",
            Self::SidecarStderr => "sidecar-stderr",
            Self::PluginStderr => "plugin-stderr",
            Self::DesktopError => "desktop-error",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticModeSnapshot {
    pub enabled: bool,
    /// False means an explicit selection could not be saved and is active
    /// for this session only. A missing preference is still persistently
    /// safe: the next launch defaults to detailed capture being off.
    pub persisted: bool,
    pub has_captured_logs: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPreference {
    schema: u8,
    enabled: bool,
}

#[derive(Clone, Debug)]
struct StoragePaths {
    preference: PathBuf,
    logs: PathBuf,
}

#[derive(Debug)]
struct QueuedDetail {
    generation: u64,
    rendered: String,
}

/// App-managed mode state. It has no connection to the remote Harness
/// webview: only the controller capability is allowed to query or mutate it.
pub struct DiagnosticMode {
    enabled: Arc<AtomicBool>,
    /// Every preference transition and explicit clear moves to a new capture
    /// generation. A line that was queued before that boundary is discarded
    /// by the writer instead of leaking into a fresh investigation.
    generation: Arc<AtomicU64>,
    persisted: AtomicBool,
    storage: Option<StoragePaths>,
    /// Serializes configuration publication, clearing, and the writer's file
    /// operations. Pipe readers only take a non-blocking lease before
    /// publishing a line; a slow private disk therefore makes them drop a
    /// best-effort detail line instead of backing up a child pipe.
    update_gate: Arc<Mutex<()>>,
    sender: Option<SyncSender<QueuedDetail>>,
}

impl DiagnosticMode {
    fn new(storage: Option<StoragePaths>, enabled: bool, persisted: bool) -> Self {
        let enabled_state = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let update_gate = Arc::new(Mutex::new(()));
        // A persisted enabled preference must recreate its own log directory
        // after a benign cleanup, but it must fail closed if that path became
        // a link/reparse point. Disabled mode does not touch the log path.
        let initial_capture_storage = match storage.as_ref() {
            Some(storage) if enabled => match ensure_log_dir(&storage.logs) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("[dsh-desktop] detailed diagnostic storage unavailable: {error}");
                    false
                }
            },
            Some(_) => true,
            None => false,
        };
        // The writer itself never opens a log file until both this mode is
        // enabled and `is_real_log_dir()` rechecks the final directory. Spawn
        // it whenever private storage is known, even if a persisted opt-in
        // found a transiently bad log directory. Otherwise that one failed
        // first launch would leave `sender` permanently absent and a user
        // could not re-enable diagnostics after repairing the directory
        // without restarting Desktop.
        let sender = storage.as_ref().and_then(|storage| {
            spawn_detail_writer(
                storage.clone(),
                Arc::clone(&enabled_state),
                Arc::clone(&generation),
                Arc::clone(&update_gate),
            )
        });
        let capture_available = initial_capture_storage && sender.is_some();
        enabled_state.store(enabled && capture_available, Ordering::Release);
        Self {
            enabled: enabled_state,
            generation,
            persisted: AtomicBool::new(persisted && (!enabled || capture_available)),
            storage,
            update_gate,
            sender,
        }
    }

    pub fn snapshot(&self) -> DiagnosticModeSnapshot {
        DiagnosticModeSnapshot {
            enabled: self.enabled.load(Ordering::Acquire),
            persisted: self.persisted.load(Ordering::Acquire),
            has_captured_logs: self.has_captured_logs(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn set_enabled(&self, enabled: bool) -> Result<DiagnosticModeSnapshot, String> {
        let _gate = self
            .update_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(storage) = &self.storage else {
            return Err("detailed diagnostic storage is unavailable".to_string());
        };
        if enabled && self.sender.is_none() {
            return Err("detailed diagnostic writer is unavailable".to_string());
        }

        let was_enabled = self.enabled();
        // Exclude the writer while an opt-in boundary clears or replaces
        // local evidence. On an enable failure, restore the previous session
        // preference rather than silently disabling an active investigation.
        self.enabled.store(false, Ordering::Release);

        // Each explicit enable starts a fresh investigation. Clear first, so
        // failure cannot claim a new capture is active while exposing old
        // evidence as if it belonged to the new reproduction attempt.
        if enabled {
            if let Err(error) = ensure_log_dir(&storage.logs).and_then(|()| clear_logs_at(storage))
            {
                self.enabled.store(was_enabled, Ordering::Release);
                return Err(error);
            }
        }

        self.generation.fetch_add(1, Ordering::AcqRel);

        let persisted = match write_preference(&storage.preference, enabled) {
            Ok(()) => true,
            Err(_) => {
                eprintln!(
                    "[dsh-desktop] detailed diagnostic preference could not be persisted; using session preference"
                );
                false
            }
        };
        self.enabled.store(enabled, Ordering::Release);
        self.persisted.store(persisted, Ordering::Release);
        Ok(self.snapshot())
    }

    fn clear_logs(&self) -> Result<(), String> {
        let _gate = self
            .update_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(storage) = &self.storage else {
            return Err("detailed diagnostic storage is unavailable".to_string());
        };
        // A clear is an evidence boundary, not just a best-effort deletion.
        // Temporarily suppress new pipe lines while the writer is excluded by
        // the same gate; otherwise a line observed during deletion could be
        // queued with the new generation and reappear immediately after the
        // user chose Clear.
        let was_enabled = self.enabled();
        self.enabled.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        let result = clear_logs_at(storage);
        self.enabled.store(was_enabled, Ordering::Release);
        result
    }

    /// Best-effort only: logging must not block Harness/sidecar/plugin pipe
    /// readers. The caller has already selected an approved source enum, and
    /// only stderr/Desktop errors may enter this path.
    pub fn record_line(&self, source: DetailedLogSource, line: &str, dsh_home: &str) {
        if !self.enabled() {
            return;
        }
        let Some(sender) = &self.sender else {
            return;
        };
        let detail = sanitize_detail(&crate::redaction::redact(line, dsh_home));
        let rendered = format!(
            "{} [{}] {}\n",
            now_ms(),
            source.name(),
            if detail.is_empty() {
                "<empty>"
            } else {
                &detail
            }
        );
        // A clear/re-enable is an evidence boundary.  A line that began
        // formatting just before that boundary must never load the *new*
        // generation and land in its fresh log.  Taking this same gate as
        // the transition gives the queued entry one unambiguous side: the
        // old generation (which the writer drops) or the new generation.
        // This is deliberately `try_lock`: stderr readers must never wait on
        // a slow file append, clear, or preference write.
        let _gate = match self.update_gate.try_lock() {
            Ok(gate) => gate,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        if !self.enabled() {
            return;
        }
        // `try_send` is the only synchronisation point reached by child pipe
        // readers. If detailed output is noisier than the bounded writer can
        // persist, retain earlier evidence and drop excess lines rather than
        // stalling Harness/sidecar/plugin stderr.
        let _ = sender.try_send(QueuedDetail {
            generation: self.generation.load(Ordering::Acquire),
            rendered,
        });
    }

    pub fn evidence_files(&self) -> Vec<EvidenceFile> {
        let Some(storage) = &self.storage else {
            return Vec::new();
        };
        if !is_real_log_dir(&storage.logs) || !self.has_captured_logs() {
            return Vec::new();
        }
        let mut files = Vec::with_capacity(DETAIL_LOG_ROTATIONS + 1);
        for rotation in 0..=DETAIL_LOG_ROTATIONS {
            let (archive_name, path) = if rotation == 0 {
                (
                    "evidence/detailed/detailed-stderr.log",
                    storage.logs.join("detailed-stderr.log"),
                )
            } else {
                let archive_name = match rotation {
                    1 => "evidence/detailed/detailed-stderr.log.1",
                    _ => "evidence/detailed/detailed-stderr.log.2",
                };
                (
                    archive_name,
                    storage.logs.join(format!("detailed-stderr.log.{rotation}")),
                )
            };
            files.push(EvidenceFile {
                archive_name,
                path,
                max_bytes: DETAIL_LOG_BYTES,
            });
        }
        files
    }

    fn has_captured_logs(&self) -> bool {
        let Some(storage) = &self.storage else {
            return false;
        };
        if !is_real_log_dir(&storage.logs) {
            return false;
        }
        debug_log_paths(&storage.logs).into_iter().any(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| {
                !secure_fs::is_symlink_or_reparse(&metadata)
                    && metadata.is_file()
                    && metadata.len() > 0
            })
        })
    }
}

/// Initialize before Harness startup so the very first failed launch is
/// eligible for capture if the user opted in during an earlier session.
pub fn init(app: &AppHandle) {
    let (storage, enabled, persisted) = match storage_paths(app) {
        Ok(storage) => match read_preference(&storage.preference) {
            Ok(Some(enabled)) => (Some(storage), enabled, true),
            // There is no file on a first launch, but the default-off policy
            // is durable.  Do not present the user with a misleading
            // persistence warning merely because they have never opted in.
            Ok(None) => (Some(storage), false, true),
            Err(_) => {
                eprintln!(
                    "[dsh-desktop] detailed diagnostic preference was invalid; detailed capture remains disabled"
                );
                (Some(storage), false, false)
            }
        },
        Err(_) => {
            eprintln!(
                "[dsh-desktop] detailed diagnostic storage unavailable; detailed capture remains disabled"
            );
            (None, false, false)
        }
    };
    app.manage(DiagnosticMode::new(storage, enabled, persisted));
}

fn storage_paths(app: &AppHandle) -> Result<StoragePaths, String> {
    let paths = crate::paths::resolve(app)?;
    crate::secure_fs::ensure_private_dir(&paths.dsh_home)?;
    let tools = paths.dsh_home.join(".desktop-tools");
    crate::secure_fs::ensure_private_dir(&tools)?;
    Ok(StoragePaths {
        preference: tools.join("detailed-diagnostics.json"),
        logs: tools.join("detailed-diagnostics"),
    })
}

fn read_preference(path: &Path) -> Result<Option<bool>, String> {
    let Some(bytes) = secure_fs::read_bounded(path, PREFERENCE_MAX_BYTES as u64)? else {
        return Ok(None);
    };
    let stored: StoredPreference = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid detailed diagnostic preference".to_string())?;
    if stored.schema != PREFERENCE_SCHEMA {
        return Err("unsupported detailed diagnostic preference schema".to_string());
    }
    Ok(Some(stored.enabled))
}

fn write_preference(path: &Path, enabled: bool) -> Result<(), String> {
    let bytes = serde_json::to_vec(&StoredPreference {
        schema: PREFERENCE_SCHEMA,
        enabled,
    })
    .map_err(|_| "cannot encode detailed diagnostic preference".to_string())?;
    secure_fs::atomic_write(path, &bytes, PREFERENCE_MAX_BYTES)
}

fn ensure_log_dir(path: &Path) -> Result<(), String> {
    secure_fs::ensure_private_dir(path)
}

fn is_real_log_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| !secure_fs::is_symlink_or_reparse(&metadata) && metadata.is_dir())
}

/// Start one private writer for the controller lifetime. The sender is
/// bounded and all untrusted process output has already been redacted and
/// byte-limited before it reaches this queue.
fn spawn_detail_writer(
    storage: StoragePaths,
    enabled: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    update_gate: Arc<Mutex<()>>,
) -> Option<SyncSender<QueuedDetail>> {
    let (sender, receiver) = mpsc::sync_channel::<QueuedDetail>(DETAIL_LOG_CHANNEL_CAPACITY);
    let spawned = std::thread::Builder::new()
        .name("detailed-diagnostics".to_string())
        .spawn(move || {
            while let Ok(entry) = receiver.recv() {
                let _gate = update_gate
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // The log directory is Desktop-owned private state. Do not
                // recreate or follow it if another local process replaced
                // its final component with a link/reparse point after the
                // user opted in; losing a line is safer than writing stderr
                // somewhere unexpected.
                if !enabled.load(Ordering::Acquire)
                    || generation.load(Ordering::Acquire) != entry.generation
                    || !is_real_log_dir(&storage.logs)
                {
                    continue;
                }
                let _ = crate::observability::append_rotating(
                    &storage.logs.join("detailed-stderr.log"),
                    entry.rendered.as_bytes(),
                    DETAIL_LOG_BYTES,
                    DETAIL_LOG_ROTATIONS,
                );
            }
        });
    match spawned {
        Ok(_) => Some(sender),
        Err(error) => {
            eprintln!("[dsh-desktop] detailed diagnostic writer unavailable: {error}");
            None
        }
    }
}

fn debug_log_paths(logs: &Path) -> Vec<PathBuf> {
    let mut paths = vec![logs.join("detailed-stderr.log")];
    for rotation in 1..=DETAIL_LOG_ROTATIONS {
        paths.push(logs.join(format!("detailed-stderr.log.{rotation}")));
    }
    paths
}

fn clear_logs_at(storage: &StoragePaths) -> Result<(), String> {
    match fs::symlink_metadata(&storage.logs) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot inspect detailed diagnostic directory {}: {error}",
                storage.logs.display()
            ))
        }
        Ok(metadata) if secure_fs::is_symlink_or_reparse(&metadata) || !metadata.is_dir() => {
            return Err("detailed diagnostic directory is not a real directory".to_string())
        }
        Ok(_) => {}
    }
    for path in debug_log_paths(&storage.logs) {
        secure_fs::check_regular_or_missing(&path)?;
        if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "cannot clear detailed diagnostic log {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn sanitize_detail(text: &str) -> String {
    let mut output = String::new();
    let mut truncated = false;
    for character in text.chars() {
        let sanitized = match character {
            '\n' | '\r' => ' ',
            character if character.is_control() => '�',
            character => character,
        };
        if output.len().saturating_add(sanitized.len_utf8()) > DETAIL_LINE_BYTES {
            truncated = true;
            break;
        }
        output.push(sanitized);
    }
    if truncated && output.len().saturating_add('…'.len_utf8()) <= DETAIL_LINE_BYTES {
        output.push('…');
    }
    output
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[tauri::command]
pub fn get_diagnostic_mode(mode: State<'_, DiagnosticMode>) -> DiagnosticModeSnapshot {
    mode.snapshot()
}

#[tauri::command]
pub fn set_diagnostic_mode(
    enabled: bool,
    mode: State<'_, DiagnosticMode>,
    observability: State<'_, Arc<crate::observability::Observability>>,
) -> Result<DiagnosticModeSnapshot, String> {
    let snapshot = mode.set_enabled(enabled)?;
    observability.record(
        if enabled {
            "detailed_diagnostics_enabled"
        } else {
            "detailed_diagnostics_disabled"
        },
        serde_json::json!({ "persisted": snapshot.persisted }),
    );
    Ok(snapshot)
}

#[tauri::command]
pub fn clear_diagnostic_logs(
    mode: State<'_, DiagnosticMode>,
    observability: State<'_, Arc<crate::observability::Observability>>,
) -> Result<(), String> {
    mode.clear_logs()?;
    observability.record("detailed_diagnostics_cleared", serde_json::json!({}));
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshd-diagnostic-mode-{name}-{}",
            secure_fs::random_suffix().unwrap()
        ))
    }

    fn new_at(root: &Path) -> DiagnosticMode {
        secure_fs::ensure_private_dir(root).unwrap();
        let tools = root.join(".desktop-tools");
        secure_fs::ensure_private_dir(&tools).unwrap();
        DiagnosticMode::new(
            Some(StoragePaths {
                preference: tools.join("detailed-diagnostics.json"),
                logs: tools.join("detailed-diagnostics"),
            }),
            false,
            true,
        )
    }

    fn wait_for_capture(mode: &DiagnosticMode) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !mode.has_captured_logs() {
            assert!(
                std::time::Instant::now() < deadline,
                "detailed diagnostic writer did not persist a queued line"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn default_is_off_and_enabling_starts_a_fresh_bounded_capture() {
        let root = test_root("capture");
        let mode = new_at(&root);
        mode.record_line(
            DetailedLogSource::HarnessStderr,
            "must not persist",
            "/private/dsh",
        );
        assert!(!mode.snapshot().enabled);
        assert!(mode.snapshot().persisted);
        assert!(!mode.has_captured_logs());

        let enabled = mode.set_enabled(true).unwrap();
        assert!(enabled.enabled);
        assert!(enabled.persisted);
        mode.record_line(
            DetailedLogSource::HarnessStderr,
            "Error /private/dsh sk-abcdefghijklmnop\u{1b}[31m",
            "/private/dsh",
        );
        wait_for_capture(&mode);
        let log = fs::read_to_string(
            root.join(".desktop-tools/detailed-diagnostics/detailed-stderr.log"),
        )
        .unwrap();
        assert!(log.contains("[harness-stderr] Error <DSH_HOME> sk-***�[31m"));
        assert!(mode.has_captured_logs());
        assert_eq!(mode.evidence_files().len(), DETAIL_LOG_ROTATIONS + 1);

        mode.set_enabled(true).unwrap();
        assert!(!mode.has_captured_logs());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabling_stops_capture_and_clear_removes_existing_evidence() {
        let root = test_root("clear");
        let mode = new_at(&root);
        mode.set_enabled(true).unwrap();
        mode.record_line(DetailedLogSource::PluginStderr, "pnpm failed", "");
        wait_for_capture(&mode);
        mode.set_enabled(false).unwrap();
        mode.record_line(DetailedLogSource::PluginStderr, "must not append", "");
        let log_path = root.join(".desktop-tools/detailed-diagnostics/detailed-stderr.log");
        assert!(fs::read_to_string(&log_path)
            .unwrap()
            .contains("pnpm failed"));
        assert!(!fs::read_to_string(&log_path)
            .unwrap()
            .contains("must not append"));
        mode.clear_logs().unwrap();
        assert!(!mode.has_captured_logs());
        assert!(!log_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clear_while_enabled_starts_a_new_capture_boundary() {
        let root = test_root("clear-active");
        let mode = new_at(&root);
        mode.set_enabled(true).unwrap();
        mode.record_line(DetailedLogSource::HarnessStderr, "before clear", "");
        wait_for_capture(&mode);

        mode.clear_logs().unwrap();
        assert!(mode.enabled());
        assert!(!mode.has_captured_logs());

        mode.record_line(DetailedLogSource::HarnessStderr, "after clear", "");
        wait_for_capture(&mode);
        let log = fs::read_to_string(
            root.join(".desktop-tools/detailed-diagnostics/detailed-stderr.log"),
        )
        .unwrap();
        assert!(log.contains("after clear"));
        assert!(!log.contains("before clear"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_line_contending_with_an_evidence_boundary_is_dropped_without_blocking() {
        use std::sync::Arc;

        let root = test_root("boundary-contention");
        let mode = Arc::new(new_at(&root));
        mode.set_enabled(true).unwrap();

        // `set_enabled` and `clear_logs` hold this gate while advancing the
        // generation and clearing the files. A pipe reader must neither wait
        // for that work nor publish a line on the wrong side of the boundary.
        let gate = mode
            .update_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let contender = Arc::clone(&mode);
        let reader = std::thread::spawn(move || {
            contender.record_line(
                DetailedLogSource::HarnessStderr,
                "must not cross diagnostic boundary",
                "",
            );
        });
        reader.join().unwrap();
        drop(gate);

        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !mode.has_captured_logs(),
            "a contended pipe reader must drop instead of crossing a clear/re-enable boundary"
        );

        mode.record_line(DetailedLogSource::HarnessStderr, "fresh evidence", "");
        wait_for_capture(&mode);
        let log = fs::read_to_string(
            root.join(".desktop-tools/detailed-diagnostics/detailed-stderr.log"),
        )
        .unwrap();
        assert!(log.contains("fresh evidence"));
        assert!(!log.contains("must not cross diagnostic boundary"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detail_line_is_bounded_by_bytes_without_breaking_utf8() {
        let line = "中".repeat(DETAIL_LINE_BYTES);
        let sanitized = sanitize_detail(&line);
        assert!(sanitized.len() <= DETAIL_LINE_BYTES);
        assert!(sanitized.is_char_boundary(sanitized.len()));
        assert!(sanitized
            .chars()
            .all(|character| character == '中' || character == '…'));
    }

    #[cfg(unix)]
    #[test]
    fn clear_refuses_a_symlinked_log_leaf() {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink");
        let mode = new_at(&root);
        let logs = root.join(".desktop-tools/detailed-diagnostics");
        ensure_log_dir(&logs).unwrap();
        let target = root.join("outside");
        fs::write(&target, b"preserve").unwrap();
        symlink(&target, logs.join("detailed-stderr.log")).unwrap();
        assert!(mode.clear_logs().is_err());
        assert_eq!(fs::read(&target).unwrap(), b"preserve");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn capture_and_export_ignore_a_replaced_log_directory() {
        use std::os::unix::fs::symlink;

        let root = test_root("directory-symlink");
        let mode = new_at(&root);
        mode.set_enabled(true).unwrap();
        let logs = root.join(".desktop-tools/detailed-diagnostics");
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        let outside_log = outside.join("detailed-stderr.log");
        fs::write(&outside_log, b"preserve").unwrap();
        fs::remove_dir(&logs).unwrap();
        symlink(&outside, &logs).unwrap();

        mode.record_line(DetailedLogSource::HarnessStderr, "must not write", "");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(fs::read(&outside_log).unwrap(), b"preserve");
        assert!(!mode.has_captured_logs());
        assert!(mode.evidence_files().is_empty());
        assert!(mode.clear_logs().is_err());

        fs::remove_file(&logs).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn initial_unavailable_log_directory_can_be_repaired_without_restart() {
        use std::os::unix::fs::symlink;

        let root = test_root("repair-after-initial-unavailable");
        secure_fs::ensure_private_dir(&root).unwrap();
        let tools = root.join(".desktop-tools");
        secure_fs::ensure_private_dir(&tools).unwrap();
        let logs = tools.join("detailed-diagnostics");
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &logs).unwrap();

        let mode = DiagnosticMode::new(
            Some(StoragePaths {
                preference: tools.join("detailed-diagnostics.json"),
                logs: logs.clone(),
            }),
            true,
            true,
        );
        assert!(!mode.snapshot().enabled);
        assert!(!mode.snapshot().persisted);

        // A local repair removes the untrusted link. The already-running
        // writer remains inert until `set_enabled` recreates a real private
        // directory and raises the generation.
        fs::remove_file(&logs).unwrap();
        assert!(mode.set_enabled(true).unwrap().enabled);
        mode.record_line(DetailedLogSource::DesktopError, "repaired", "");
        wait_for_capture(&mode);
        assert!(fs::read_to_string(logs.join("detailed-stderr.log"))
            .unwrap()
            .contains("repaired"));
        fs::remove_dir_all(root).unwrap();
    }
}
