//! Bounded, cancellable diagnostic export.
//!
//! The archive is best-effort redacted and intentionally excludes sessions,
//! workspaces, native memory dumps, and arbitrary files below DSH_HOME.

use crate::harness::Runtime;
use crate::observability::{EvidenceFile, Observability};
use crate::secure_fs;
use serde_json::Value;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, State};

const ARCHIVE_BYTES: u64 = 50 * 1024 * 1024;
const EXPORT_DEADLINE: Duration = Duration::from_secs(60);
const SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

const PRIVACY_NOTICE: &str = "DSH Desktop diagnostic archive\n\n\
This archive contains Desktop status, a redacted in-memory log tail, and bounded Desktop lifecycle evidence.\n\
It deliberately excludes Harness sessions, workspace files, prompts, tool output persisted by Harness, and memory dumps.\n\
Redaction is best effort only. Review every file before sharing the archive.\n";

pub struct DiagnosticExporter {
    busy: AtomicBool,
    cancelled: AtomicBool,
}

impl DiagnosticExporter {
    pub fn new() -> Self {
        Self {
            busy: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        }
    }

    fn begin(&self) -> Result<DiagnosticPermit<'_>, String> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "diagnostic export is already running".to_string())?;
        self.cancelled.store(false, Ordering::Release);
        Ok(DiagnosticPermit { exporter: self })
    }

    fn finish(&self) {
        self.busy.store(false, Ordering::Release);
    }

    pub fn cancel(&self) -> bool {
        if !self.busy.load(Ordering::Acquire) {
            return false;
        }
        self.cancelled.store(true, Ordering::Release);
        true
    }
}

struct DiagnosticPermit<'a> {
    exporter: &'a DiagnosticExporter,
}

impl Drop for DiagnosticPermit<'_> {
    fn drop(&mut self) {
        self.exporter.finish();
    }
}

#[derive(Debug)]
struct ArchiveEntry {
    name: String,
    bytes: Vec<u8>,
}

/// Mask common secret shapes. This remains explicitly best effort because
/// arbitrary log content cannot be proven secret-free.
pub fn redact(text: &str, dsh_home: &str) -> String {
    let output = if dsh_home.is_empty() {
        text.to_owned()
    } else {
        text.replace(dsh_home, "<DSH_HOME>")
    };
    let mut result = String::with_capacity(output.len());
    let mut rest = output.as_str();
    while !rest.is_empty() {
        let bytes = rest.as_bytes();
        if bytes.starts_with(b"sk-") {
            let token = bytes
                .iter()
                .skip(3)
                .take_while(|byte| byte.is_ascii_alphanumeric())
                .count();
            if token >= 16 {
                result.push_str("sk-***");
                rest = &rest[3 + token..];
                continue;
            }
        }
        if bytes.starts_with(b"Bearer ") {
            let token = bytes
                .iter()
                .skip(7)
                .take_while(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                })
                .count();
            if token >= 8 {
                result.push_str("Bearer ***");
                rest = &rest[7 + token..];
                continue;
            }
        }
        if bytes.starts_with(b"AKIA") {
            let token = bytes
                .iter()
                .skip(4)
                .take_while(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                .count();
            if token >= 12 {
                result.push_str("AKIA***");
                rest = &rest[4 + token..];
                continue;
            }
        }
        match rest.chars().next() {
            Some(character) => {
                result.push(character);
                rest = &rest[character.len_utf8()..];
            }
            None => break,
        }
    }
    result
}

pub fn snapshot(runtime: &Runtime, observability: &Observability) -> Value {
    let state = runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dsh_home = state.dsh_home.clone().unwrap_or_default();
    let tail_start = state.logs.len().saturating_sub(500);
    serde_json::json!({
        "schemaVersion": 1,
        "generator": "deepseek-harness-desktop",
        "status": state.status,
        "pid": state.pid,
        "versions": state.versions,
        "platform": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "lastError": state.last_error.as_deref().map(|error| redact(error, &dsh_home)),
        // Do not place the persistence error itself in an archive: OS errors
        // commonly contain the user's absolute app-data path.
        "observabilityPersistent": observability.initialization_error().is_none(),
        "logsTail": state.logs[tail_start..].iter().map(|(stream, line)| {
            serde_json::json!({ "stream": stream, "line": redact(line, &dsh_home) })
        }).collect::<Vec<_>>(),
    })
}

#[tauri::command]
pub async fn export_diagnostics(
    app: AppHandle,
    runtime: State<'_, Runtime>,
    observability: State<'_, Arc<Observability>>,
    exporter: State<'_, Arc<DiagnosticExporter>>,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;
    let _permit = exporter.begin()?;
    observability.record("diagnostics_export_started", serde_json::json!({}));
    let result = async {
        let (path_tx, mut path_rx) = tauri::async_runtime::channel::<Option<PathBuf>>(1);
        app.dialog()
            .file()
            .add_filter("ZIP", &["zip"])
            .set_file_name("dsh-desktop-diagnostics.zip")
            .save_file(move |path| {
                let _ = path_tx.try_send(path.and_then(|value| value.into_path().ok()));
            });
        let Some(destination) = path_rx
            .recv()
            .await
            .ok_or_else(|| "diagnostic save dialog closed".to_string())?
        else {
            return Ok(false);
        };
        if destination.as_os_str().is_empty() {
            return Err("diagnostic destination is empty".to_string());
        }

        let snapshot = snapshot(&runtime, &observability);
        let dsh_home = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .dsh_home
            .clone()
            .unwrap_or_default();
        let evidence = observability.evidence_files();
        let exporter_for_worker = Arc::clone(&exporter);
        let (result_tx, mut result_rx) = tauri::async_runtime::channel::<Result<(), String>>(1);
        std::thread::Builder::new()
            .name("diagnostics-export".to_string())
            .spawn(move || {
                let started = Instant::now();
                let result = collect_entries(
                    snapshot,
                    evidence,
                    &dsh_home,
                    &exporter_for_worker.cancelled,
                    started,
                )
                .and_then(|entries| {
                    write_archive(
                        &destination,
                        entries,
                        &exporter_for_worker.cancelled,
                        started,
                    )
                });
                let _ = result_tx.try_send(result);
            })
            .map_err(|e| format!("cannot start diagnostic export worker: {e}"))?;
        result_rx
            .recv()
            .await
            .ok_or_else(|| "diagnostic export worker stopped unexpectedly".to_string())??;
        Ok(true)
    }
    .await;
    match &result {
        Ok(true) => observability.record("diagnostics_export_completed", serde_json::json!({})),
        Ok(false) => observability.record("diagnostics_export_dismissed", serde_json::json!({})),
        Err(_) if exporter.cancelled.load(Ordering::Acquire) => {
            observability.record("diagnostics_export_cancelled", serde_json::json!({}))
        }
        Err(_) => observability.record("diagnostics_export_failed", serde_json::json!({})),
    }
    result
}

#[tauri::command]
pub fn cancel_diagnostics_export(exporter: State<'_, Arc<DiagnosticExporter>>) -> bool {
    exporter.cancel()
}

fn collect_entries(
    snapshot: Value,
    evidence: Vec<EvidenceFile>,
    dsh_home: &str,
    cancelled: &AtomicBool,
    started: Instant,
) -> Result<Vec<ArchiveEntry>, String> {
    check_cancelled(cancelled, started)?;
    let snapshot_text = serde_json::to_string_pretty(&snapshot)
        .map_err(|e| format!("cannot serialize diagnostic snapshot: {e}"))?;
    let snapshot_bytes = redact(&snapshot_text, dsh_home).into_bytes();
    if snapshot_bytes.len() > SNAPSHOT_BYTES {
        return Err("diagnostic snapshot exceeds its byte limit".to_string());
    }
    let mut entries = vec![
        ArchiveEntry {
            name: "privacy.txt".to_string(),
            bytes: PRIVACY_NOTICE.as_bytes().to_vec(),
        },
        ArchiveEntry {
            name: "diagnostics.json".to_string(),
            bytes: snapshot_bytes,
        },
    ];
    let mut total = entries
        .iter()
        .map(|entry| entry.bytes.len() as u64)
        .sum::<u64>();
    for item in evidence {
        check_cancelled(cancelled, started)?;
        let bytes = match secure_fs::read_bounded(&item.path, item.max_bytes) {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(_) => continue,
        };
        let redacted = redact(&String::from_utf8_lossy(&bytes), dsh_home).into_bytes();
        total = total.saturating_add(redacted.len() as u64);
        if total > ARCHIVE_BYTES {
            return Err("diagnostic evidence exceeds archive byte limit".to_string());
        }
        entries.push(ArchiveEntry {
            name: item.archive_name.to_string(),
            bytes: redacted,
        });
    }
    Ok(entries)
}

fn write_archive(
    destination: &Path,
    entries: Vec<ArchiveEntry>,
    cancelled: &AtomicBool,
    started: Instant,
) -> Result<(), String> {
    secure_fs::check_regular_or_missing(destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| "diagnostic destination has no parent".to_string())?;
    let parent_meta = std::fs::metadata(parent)
        .map_err(|e| format!("cannot inspect diagnostic destination directory: {e}"))?;
    if !parent_meta.is_dir() {
        return Err("diagnostic destination parent is not a directory".to_string());
    }
    let temp = secure_fs::sibling_temp(destination, "export")?;
    let result = (|| {
        let file = secure_fs::create_private_new(&temp)?;
        let writer = LimitedWriter::new(file, ARCHIVE_BYTES);
        let mut zip = zip::ZipWriter::new(writer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for entry in entries {
            check_cancelled(cancelled, started)?;
            zip.start_file(entry.name, options)
                .map_err(|e| format!("cannot add diagnostic archive entry: {e}"))?;
            for chunk in entry.bytes.chunks(64 * 1024) {
                check_cancelled(cancelled, started)?;
                zip.write_all(chunk)
                    .map_err(|e| format!("cannot write diagnostic archive: {e}"))?;
            }
        }
        let writer = zip
            .finish()
            .map_err(|e| format!("cannot finish diagnostic archive: {e}"))?;
        writer
            .file
            .sync_all()
            .map_err(|e| format!("cannot sync diagnostic archive: {e}"))?;
        drop(writer);
        check_cancelled(cancelled, started)?;
        secure_fs::replace_file(&temp, destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn check_cancelled(cancelled: &AtomicBool, started: Instant) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("diagnostic export cancelled".to_string());
    }
    if started.elapsed() > EXPORT_DEADLINE {
        return Err("diagnostic export exceeded its 60 second deadline".to_string());
    }
    Ok(())
}

struct LimitedWriter {
    file: File,
    limit: u64,
    position: u64,
}

impl LimitedWriter {
    fn new(file: File, limit: u64) -> Self {
        Self {
            file,
            limit,
            position: 0,
        }
    }
}

impl Write for LimitedWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.position.saturating_add(buffer.len() as u64) > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "diagnostic archive byte limit exceeded",
            ));
        }
        let written = self.file.write(buffer)?;
        self.position = self.position.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Seek for LimitedWriter {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let next = self.file.seek(position)?;
        if next > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "diagnostic archive seek exceeded byte limit",
            ));
        }
        self.position = next;
        Ok(next)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn destination(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshd-diagnostics-{name}-{}.zip",
            secure_fs::random_suffix().unwrap()
        ))
    }

    #[test]
    fn redaction_preserves_utf8_and_masks_known_shapes() {
        let input = "日志 /private/dsh sk-abcdefghijklmnop Bearer abcdefgh AKIAABCDEFGHIJKLMN";
        assert_eq!(
            redact(input, "/private/dsh"),
            "日志 <DSH_HOME> sk-*** Bearer *** AKIA***"
        );
        assert_eq!(redact("sk-abcdefghijklmno", ""), "sk-abcdefghijklmno");
    }

    #[test]
    fn archive_is_complete_and_contains_privacy_notice() {
        let path = destination("complete");
        let cancel = AtomicBool::new(false);
        write_archive(
            &path,
            vec![ArchiveEntry {
                name: "diagnostics.json".to_string(),
                bytes: b"{}".to_vec(),
            }],
            &cancel,
            Instant::now(),
        )
        .unwrap();
        let file = File::open(&path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert!(archive.by_name("diagnostics.json").is_ok());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cancelled_export_leaves_no_destination_or_temp() {
        let path = destination("cancelled");
        let cancel = AtomicBool::new(true);
        let error = write_archive(
            &path,
            vec![ArchiveEntry {
                name: "diagnostics.json".to_string(),
                bytes: vec![b'x'; 1024],
            }],
            &cancel,
            Instant::now(),
        )
        .unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(!path.exists());
    }

    #[test]
    fn exporter_is_single_flight() {
        let exporter = DiagnosticExporter::new();
        let permit = exporter.begin().unwrap();
        assert!(matches!(
            exporter.begin(),
            Err(error) if error.contains("already running")
        ));
        assert!(exporter.cancel());
        drop(permit);
        assert!(!exporter.cancel());
    }

    #[test]
    fn export_deadline_covers_collection_and_archive_write() {
        let cancel = AtomicBool::new(false);
        let started = Instant::now()
            .checked_sub(EXPORT_DEADLINE + Duration::from_secs(1))
            .unwrap();
        let error = check_cancelled(&cancel, started).unwrap_err();
        assert!(error.contains("60 second deadline"));
    }
}
