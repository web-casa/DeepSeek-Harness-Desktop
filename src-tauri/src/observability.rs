//! Privacy-preserving Desktop lifecycle evidence.
//!
//! This intentionally never persists Harness stdout/stderr, prompts, tool
//! output, sessions, or workspace paths. It records only bounded shell-owned
//! state transitions that are useful for proving whether startup and shutdown
//! completed.

use crate::secure_fs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Manager;

const LOG_BYTES: u64 = 5 * 1024 * 1024;
const LOG_ROTATIONS: usize = 3;
const LIFECYCLE_BYTES: u64 = 1024 * 1024;
const EVENT_BYTES: usize = 4096;
const MARKER_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
struct PersistentState {
    root: PathBuf,
    run_id: String,
}

/// The object always exists. If the platform app-data path is unavailable or
/// cannot be protected, persistence is disabled and the reason is included in
/// the explicit diagnostic snapshot instead of aborting Harness startup.
pub struct Observability {
    persistent: Option<PersistentState>,
    init_error: Option<String>,
    write_lock: Mutex<()>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunMarker {
    schema_version: u32,
    run_id: String,
    pid: u32,
    started_at_ms: u64,
    clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleEvent<'a> {
    schema_version: u32,
    run_id: &'a str,
    timestamp_ms: u64,
    name: &'a str,
    details: Value,
}

#[derive(Debug, Clone)]
pub struct EvidenceFile {
    pub archive_name: &'static str,
    pub path: PathBuf,
    pub max_bytes: u64,
}

impl Observability {
    pub fn new(app: &tauri::AppHandle) -> Self {
        let root = app
            .path()
            .app_data_dir()
            .map(|path| path.join("desktop-state"))
            .map_err(|e| format!("cannot resolve Desktop app-data directory: {e}"));
        match root.and_then(Self::initialize) {
            Ok(persistent) => {
                let this = Self {
                    persistent: Some(persistent),
                    init_error: None,
                    write_lock: Mutex::new(()),
                };
                this.record(
                    "desktop_started",
                    serde_json::json!({
                        "platform": std::env::consts::OS,
                        "arch": std::env::consts::ARCH,
                    }),
                );
                this
            }
            Err(error) => Self {
                persistent: None,
                init_error: Some(error),
                write_lock: Mutex::new(()),
            },
        }
    }

    fn initialize(root: PathBuf) -> Result<PersistentState, String> {
        secure_fs::ensure_private_dir(&root)?;
        secure_fs::ensure_private_dir(&root.join("logs"))?;
        secure_fs::ensure_private_dir(&root.join("runs"))?;
        let run_id = secure_fs::random_suffix()?;

        let active = root.join("runs/active.json");
        if let Some(bytes) = secure_fs::read_bounded(&active, MARKER_BYTES as u64)? {
            // A clean marker can remain only if the final rename was
            // interrupted. Preserve it as last-run, not as a false crash.
            let clean = serde_json::from_slice::<RunMarker>(&bytes)
                .is_ok_and(|marker| marker.schema_version == 1 && marker.clean);
            let name = if clean {
                "runs/last-run.json"
            } else {
                "runs/previous-unclean.json"
            };
            secure_fs::atomic_write(&root.join(name), &bytes, MARKER_BYTES)?;
        }

        let lifecycle = root.join("lifecycle.jsonl");
        if lifecycle.exists() {
            secure_fs::check_regular_or_missing(&lifecycle)?;
            secure_fs::replace_file(&lifecycle, &root.join("lifecycle.previous.jsonl"))?;
        }

        let marker = RunMarker {
            schema_version: 1,
            run_id: run_id.clone(),
            pid: std::process::id(),
            started_at_ms: now_ms(),
            clean: false,
            finished_at_ms: None,
        };
        let marker_bytes = serde_json::to_vec_pretty(&marker)
            .map_err(|e| format!("cannot serialize Desktop run marker: {e}"))?;
        secure_fs::atomic_write(&active, &marker_bytes, MARKER_BYTES)?;
        Ok(PersistentState { root, run_id })
    }

    pub fn initialization_error(&self) -> Option<&str> {
        self.init_error.as_deref()
    }

    pub fn record(&self, name: &str, details: Value) {
        let Some(persistent) = &self.persistent else {
            return;
        };
        let Ok(_guard) = self.write_lock.lock() else {
            return;
        };
        let event = LifecycleEvent {
            schema_version: 1,
            run_id: &persistent.run_id,
            timestamp_ms: now_ms(),
            name,
            details,
        };
        let mut bytes = match serde_json::to_vec(&event) {
            Ok(bytes) if bytes.len() <= EVENT_BYTES.saturating_sub(1) => bytes,
            Ok(_) => match serde_json::to_vec(&LifecycleEvent {
                schema_version: 1,
                run_id: &persistent.run_id,
                timestamp_ms: now_ms(),
                name: "event_omitted",
                details: serde_json::json!({ "reason": "event exceeded byte limit" }),
            }) {
                Ok(bytes) => bytes,
                Err(_) => return,
            },
            Err(_) => return,
        };
        bytes.push(b'\n');
        let lifecycle = persistent.root.join("lifecycle.jsonl");
        let _ = append_rotating(&lifecycle, &bytes, LIFECYCLE_BYTES, 1);

        // The rolling shell log is deliberately redundant and human-readable,
        // but contains only the event name—not arbitrary JSON detail values.
        let line = format!("{} {}\n", now_ms(), sanitize_event_name(name));
        let _ = append_rotating(
            &persistent.root.join("logs/desktop.log"),
            line.as_bytes(),
            LOG_BYTES,
            LOG_ROTATIONS,
        );
    }

    pub fn mark_clean(&self) -> Result<(), String> {
        let Some(persistent) = &self.persistent else {
            return Ok(());
        };
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "Desktop evidence lock is poisoned".to_string())?;
        let active = persistent.root.join("runs/active.json");
        let bytes = secure_fs::read_bounded(&active, MARKER_BYTES as u64)?
            .ok_or_else(|| "Desktop active run marker is missing".to_string())?;
        let mut marker: RunMarker = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Desktop active run marker is invalid: {e}"))?;
        if marker.run_id != persistent.run_id || marker.pid != std::process::id() {
            return Err("Desktop active run marker ownership changed".to_string());
        }
        marker.clean = true;
        marker.finished_at_ms = Some(now_ms());
        let finished = serde_json::to_vec_pretty(&marker)
            .map_err(|e| format!("cannot serialize finished Desktop run marker: {e}"))?;
        secure_fs::atomic_write(&active, &finished, MARKER_BYTES)?;
        secure_fs::replace_file(&active, &persistent.root.join("runs/last-run.json"))
    }

    pub fn evidence_files(&self) -> Vec<EvidenceFile> {
        let Some(persistent) = &self.persistent else {
            return Vec::new();
        };
        let mut files = vec![
            EvidenceFile {
                archive_name: "evidence/lifecycle.jsonl",
                path: persistent.root.join("lifecycle.jsonl"),
                max_bytes: LIFECYCLE_BYTES,
            },
            EvidenceFile {
                archive_name: "evidence/lifecycle.previous.jsonl",
                path: persistent.root.join("lifecycle.previous.jsonl"),
                max_bytes: LIFECYCLE_BYTES,
            },
            EvidenceFile {
                archive_name: "evidence/runs/active.json",
                path: persistent.root.join("runs/active.json"),
                max_bytes: MARKER_BYTES as u64,
            },
            EvidenceFile {
                archive_name: "evidence/runs/previous-unclean.json",
                path: persistent.root.join("runs/previous-unclean.json"),
                max_bytes: MARKER_BYTES as u64,
            },
            EvidenceFile {
                archive_name: "evidence/runs/last-run.json",
                path: persistent.root.join("runs/last-run.json"),
                max_bytes: MARKER_BYTES as u64,
            },
        ];
        for rotation in 0..=LOG_ROTATIONS {
            let (archive_name, path) = if rotation == 0 {
                (
                    "evidence/logs/desktop.log",
                    persistent.root.join("logs/desktop.log"),
                )
            } else {
                let archive_name = match rotation {
                    1 => "evidence/logs/desktop.log.1",
                    2 => "evidence/logs/desktop.log.2",
                    _ => "evidence/logs/desktop.log.3",
                };
                (
                    archive_name,
                    persistent.root.join(format!("logs/desktop.log.{rotation}")),
                )
            };
            files.push(EvidenceFile {
                archive_name,
                path,
                max_bytes: LOG_BYTES,
            });
        }
        files
    }

    #[cfg(test)]
    fn new_at(root: PathBuf) -> Result<Self, String> {
        Ok(Self {
            persistent: Some(Self::initialize(root)?),
            init_error: None,
            write_lock: Mutex::new(()),
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn sanitize_event_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(80)
        .collect()
}

/// Append a bounded private evidence file. Other Desktop-owned diagnostic
/// components reuse this instead of open-coding rotation and accidentally
/// weakening the regular-file/reparse-point checks.
pub(crate) fn append_rotating(
    path: &Path,
    bytes: &[u8],
    max_bytes: u64,
    rotations: usize,
) -> Result<(), String> {
    let current_len = match fs::symlink_metadata(path) {
        Ok(meta) if secure_fs::is_symlink_or_reparse(&meta) || !meta.is_file() => {
            return Err(format!(
                "evidence path is not a regular file: {}",
                path.display()
            ));
        }
        Ok(meta) => meta.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(format!(
                "cannot inspect evidence file {}: {error}",
                path.display()
            ))
        }
    };
    if current_len.saturating_add(bytes.len() as u64) > max_bytes {
        rotate(path, rotations)?;
    }
    let mut file = secure_fs::open_private_append(path)?;
    file.write_all(bytes)
        .map_err(|e| format!("cannot append evidence file {}: {e}", path.display()))?;
    file.flush()
        .map_err(|e| format!("cannot flush evidence file {}: {e}", path.display()))
}

fn rotate(path: &Path, rotations: usize) -> Result<(), String> {
    if rotations == 0 {
        secure_fs::check_regular_or_missing(path)?;
        if path.exists() {
            fs::remove_file(path)
                .map_err(|e| format!("cannot reset evidence file {}: {e}", path.display()))?;
        }
        return Ok(());
    }
    for index in (1..=rotations).rev() {
        let source = if index == 1 {
            path.to_path_buf()
        } else {
            rotation_path(path, index - 1)
        };
        // Never carry a symlink/reparse point forward as a historical
        // evidence file. The leaf check is especially important on Windows,
        // where `Path::exists` follows a junction before the subsequent
        // rename would otherwise preserve it.
        secure_fs::check_regular_or_missing(&source)?;
        if !source.exists() {
            continue;
        }
        let destination = rotation_path(path, index);
        secure_fs::replace_file(&source, &destination)?;
    }
    Ok(())
}

fn rotation_path(path: &Path, index: usize) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{index}"));
    PathBuf::from(value)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshd-observability-{name}-{}",
            secure_fs::random_suffix().unwrap()
        ))
    }

    #[test]
    fn previous_active_marker_is_preserved_as_unclean() {
        let root = test_root("unclean");
        let first = Observability::new_at(root.clone()).unwrap();
        let first_id = first.persistent.as_ref().unwrap().run_id.clone();
        drop(first);
        let second = Observability::new_at(root.clone()).unwrap();
        let bytes = secure_fs::read_bounded(
            &root.join("runs/previous-unclean.json"),
            MARKER_BYTES as u64,
        )
        .unwrap()
        .unwrap();
        let marker: RunMarker = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(marker.run_id, first_id);
        second.mark_clean().unwrap();
        assert!(!root.join("runs/active.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_clean_marker_is_not_reported_as_a_crash() {
        let root = test_root("clean-interrupted");
        let first = Observability::new_at(root.clone()).unwrap();
        let first_id = first.persistent.as_ref().unwrap().run_id.clone();
        let active = root.join("runs/active.json");
        let bytes = secure_fs::read_bounded(&active, MARKER_BYTES as u64)
            .unwrap()
            .unwrap();
        let mut marker: RunMarker = serde_json::from_slice(&bytes).unwrap();
        marker.clean = true;
        marker.finished_at_ms = Some(now_ms());
        secure_fs::atomic_write(
            &active,
            &serde_json::to_vec_pretty(&marker).unwrap(),
            MARKER_BYTES,
        )
        .unwrap();
        drop(first);

        let second = Observability::new_at(root.clone()).unwrap();
        assert!(!root.join("runs/previous-unclean.json").exists());
        let last = secure_fs::read_bounded(&root.join("runs/last-run.json"), MARKER_BYTES as u64)
            .unwrap()
            .unwrap();
        let marker: RunMarker = serde_json::from_slice(&last).unwrap();
        assert_eq!(marker.run_id, first_id);
        assert!(marker.clean);
        second.mark_clean().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lifecycle_event_is_bounded_and_raw_detail_is_not_in_shell_log() {
        let root = test_root("event");
        let observability = Observability::new_at(root.clone()).unwrap();
        observability.record(
            "harness_ready",
            serde_json::json!({ "secret": "sk-abcdefghijklmnop" }),
        );
        let lifecycle = fs::read(root.join("lifecycle.jsonl")).unwrap();
        assert!(lifecycle.len() <= EVENT_BYTES);
        let log = fs::read_to_string(root.join("logs/desktop.log")).unwrap();
        assert!(log.contains("harness_ready"));
        assert!(!log.contains("sk-"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn append_rotates_at_bound() {
        let root = test_root("rotation");
        secure_fs::ensure_private_dir(&root).unwrap();
        let path = root.join("desktop.log");
        append_rotating(&path, b"1234", 5, 2).unwrap();
        append_rotating(&path, b"56", 5, 2).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"56");
        assert_eq!(fs::read(root.join("desktop.log.1")).unwrap(), b"1234");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rotation_refuses_a_symlinked_historical_leaf() {
        use std::os::unix::fs::symlink;

        let root = test_root("rotation-symlink");
        secure_fs::ensure_private_dir(&root).unwrap();
        let path = root.join("desktop.log");
        let outside = root.join("outside");
        fs::write(&outside, b"preserve").unwrap();
        symlink(&outside, root.join("desktop.log.1")).unwrap();

        append_rotating(&path, b"1234", 5, 2).unwrap();
        assert!(append_rotating(&path, b"56", 5, 2).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"preserve");
        fs::remove_dir_all(root).unwrap();
    }
}
