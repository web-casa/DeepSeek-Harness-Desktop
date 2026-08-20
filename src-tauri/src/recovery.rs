//! Explicit, journaled recovery for a plugin implicated in terminal startup.
//!
//! Log parsing is candidate discovery only. A package may be offered for
//! recovery only when it is also an exact dependency and active bundle in the
//! current web profile. The transaction removes only that bundle entry; it
//! never deletes a dependency, package tree, lockfile row, or Cordis patch.

use crate::plugins::{self, MarketPendingPlugin};
use crate::secure_fs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

const JOURNAL_SCHEMA: u32 = 1;
const JOURNAL_MAX_BYTES: usize = 64 * 1024;
const PROFILE_MAX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SIGNAL_LINE_BYTES: usize = 16 * 1024;
const MAX_SIGNAL_LINES: usize = 500;

// Every recovery file and profile transition belongs to one state machine.
// The PluginRunner busy flag serializes user-triggered mutations, but readers
// (`overview`) and the sidecar ready callback run on independent threads. Keep
// reconciliation and transaction creation indivisible inside this process so
// an observer cannot mistake a live Prepared journal for an interrupted one.
static RECOVERY_LOCK: Mutex<()> = Mutex::new(());

fn lock_recovery() -> MutexGuard<'static, ()> {
    RECOVERY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCandidate {
    pub package_name: String,
    pub version_spec: String,
    pub signals: Vec<String>,
    pub market_managed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryPhase {
    Prepared,
    DisabledAwaitingBoot,
    Isolated,
    RollbackPrepared,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryTransactionView {
    pub transaction_id: String,
    pub package_name: String,
    pub phase: RecoveryPhase,
    pub signals: Vec<String>,
    pub market_managed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryOverview {
    pub terminal_startup_failure: bool,
    pub candidates: Vec<RecoveryCandidate>,
    pub transaction: Option<RecoveryTransactionView>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryJournal {
    schema_version: u32,
    transaction_id: String,
    package_name: String,
    phase: RecoveryPhase,
    created_at_ms: u64,
    signals: Vec<String>,
    market_receipt: Option<MarketPendingPlugin>,
}

struct RecoveryPaths {
    active: PathBuf,
    journal: PathBuf,
    before: PathBuf,
    disabled: PathBuf,
}

pub fn overview(
    dsh_home: &Path,
    logs: &[(String, String)],
    terminal_startup_failure: bool,
) -> Result<RecoveryOverview, String> {
    let _guard = lock_recovery();
    let transaction = reconcile(dsh_home)?.map(|journal| transaction_view(&journal));
    let candidates = if terminal_startup_failure && transaction.is_none() {
        detect_candidates(dsh_home, logs)?
    } else {
        Vec::new()
    };
    Ok(RecoveryOverview {
        terminal_startup_failure,
        candidates,
        transaction,
    })
}

pub fn begin(
    dsh_home: &Path,
    logs: &[(String, String)],
    terminal_startup_failure: bool,
    package_name: &str,
) -> Result<RecoveryTransactionView, String> {
    let _guard = lock_recovery();
    begin_locked(
        dsh_home,
        logs,
        terminal_startup_failure,
        package_name,
        || {},
    )
}

fn begin_locked(
    dsh_home: &Path,
    logs: &[(String, String)],
    terminal_startup_failure: bool,
    package_name: &str,
    after_prepared: impl FnOnce(),
) -> Result<RecoveryTransactionView, String> {
    if !terminal_startup_failure {
        return Err("plugin recovery requires a terminal startup failure".to_string());
    }
    if reconcile(dsh_home)?.is_some() {
        return Err("a plugin recovery transaction is already active".to_string());
    }
    let candidate = detect_candidates(dsh_home, logs)?
        .into_iter()
        .find(|candidate| candidate.package_name == package_name)
        .ok_or_else(|| "package is not an exact active recovery candidate".to_string())?;

    let (profile, before, mut manifest) = read_recovery_manifest(dsh_home)?;
    let bundles = plugins::profile_bundles_mut(&mut manifest)?;
    if bundles.iter().any(|bundle| bundle.as_str().is_none()) {
        return Err("web profile bundles contains a non-string value".to_string());
    }
    let before_len = bundles.len();
    bundles.retain(|bundle| bundle.as_str() != Some(package_name));
    if bundles.len() == before_len {
        return Err("recovery candidate is no longer active".to_string());
    }
    let disabled = serialize_manifest(&manifest)?;
    let paths = recovery_paths(dsh_home, true)?;
    secure_fs::atomic_write(&paths.before, &before, PROFILE_MAX_BYTES as usize)?;
    secure_fs::atomic_write(&paths.disabled, &disabled, PROFILE_MAX_BYTES as usize)?;

    let mut journal = RecoveryJournal {
        schema_version: JOURNAL_SCHEMA,
        transaction_id: secure_fs::random_suffix()?,
        package_name: package_name.to_string(),
        phase: RecoveryPhase::Prepared,
        created_at_ms: unix_time_ms(),
        signals: candidate.signals,
        market_receipt: plugins::active_market_receipt(dsh_home, package_name)?,
    };
    write_journal(&paths, &journal)?;
    after_prepared();
    plugins::write_profile_manifest(&profile, &manifest)?;
    if read_profile_bytes(&profile)? != disabled {
        return Err("recovery pre-disable did not produce the expected profile".to_string());
    }
    journal.phase = RecoveryPhase::DisabledAwaitingBoot;
    write_journal(&paths, &journal)?;
    Ok(transaction_view(&journal))
}

/// Mark a pre-disabled transaction healthy only after Harness emits a newly
/// validated ready event. The exact disabled bytes must still match.
pub fn commit_after_ready(dsh_home: &Path) -> Result<(), String> {
    let _guard = lock_recovery();
    let Some(mut journal) = reconcile(dsh_home)? else {
        return Ok(());
    };
    if journal.phase != RecoveryPhase::DisabledAwaitingBoot {
        return Ok(());
    }
    let paths = recovery_paths(dsh_home, false)?;
    require_current_matches(dsh_home, &paths.disabled, "disabled recovery profile")?;
    journal.phase = RecoveryPhase::Isolated;
    write_journal(&paths, &journal)
}

pub fn rollback_receipt(
    dsh_home: &Path,
    transaction_id: &str,
) -> Result<Option<MarketPendingPlugin>, String> {
    let _guard = lock_recovery();
    let journal = require_transaction(dsh_home, transaction_id)?;
    if !matches!(
        journal.phase,
        RecoveryPhase::DisabledAwaitingBoot | RecoveryPhase::Isolated
    ) {
        return Err("recovery transaction is not rollback-ready".to_string());
    }
    Ok(journal.market_receipt)
}

pub fn rollback(dsh_home: &Path, transaction_id: &str) -> Result<(), String> {
    let _guard = lock_recovery();
    let mut journal = require_transaction(dsh_home, transaction_id)?;
    if !matches!(
        journal.phase,
        RecoveryPhase::DisabledAwaitingBoot | RecoveryPhase::Isolated
    ) {
        return Err("recovery transaction is not rollback-ready".to_string());
    }
    let paths = recovery_paths(dsh_home, false)?;
    require_current_matches(dsh_home, &paths.disabled, "disabled recovery profile")?;
    let before = secure_fs::read_bounded(&paths.before, PROFILE_MAX_BYTES)?
        .ok_or_else(|| "recovery backup is missing".to_string())?;
    validate_backup(&before, &journal.package_name)?;

    journal.phase = RecoveryPhase::RollbackPrepared;
    write_journal(&paths, &journal)?;
    write_profile_bytes(dsh_home, &before)?;
    if read_current_profile(dsh_home)? != before {
        return Err("recovery rollback did not restore the exact backup".to_string());
    }
    cleanup(&paths)
}

/// Accept the isolation and discard the rollback backup. The current profile
/// must still be exactly the one this transaction wrote.
pub fn finalize(dsh_home: &Path, transaction_id: &str) -> Result<(), String> {
    let _guard = lock_recovery();
    let journal = require_transaction(dsh_home, transaction_id)?;
    if journal.phase != RecoveryPhase::Isolated {
        return Err("recovery isolation is not finalizable before a healthy boot".to_string());
    }
    let paths = recovery_paths(dsh_home, false)?;
    require_current_matches(dsh_home, &paths.disabled, "disabled recovery profile")?;
    plugins::remove_active_market_receipt(dsh_home, &journal.package_name)?;
    cleanup(&paths)
}

pub fn has_active_transaction(dsh_home: &Path) -> Result<bool, String> {
    let _guard = lock_recovery();
    Ok(reconcile(dsh_home)?.is_some())
}

fn detect_candidates(
    dsh_home: &Path,
    logs: &[(String, String)],
) -> Result<Vec<RecoveryCandidate>, String> {
    let (_, _, manifest) = read_recovery_manifest(dsh_home)?;
    let dependencies = plugins::profile_dependencies(&manifest)?;
    let active = plugins::profile_bundles(&manifest)?;
    let signals = parse_signals(logs);
    let mut candidates = Vec::new();
    for (package_name, kinds) in signals {
        if !active.contains(package_name.as_str()) {
            continue;
        }
        let Some(version_spec) = dependencies.get(&package_name).and_then(Value::as_str) else {
            continue;
        };
        candidates.push(RecoveryCandidate {
            market_managed: plugins::active_market_receipt(dsh_home, &package_name)?.is_some(),
            package_name,
            version_spec: version_spec.to_string(),
            signals: kinds.into_iter().collect(),
        });
    }
    candidates.sort_by(|left, right| left.package_name.cmp(&right.package_name));
    Ok(candidates)
}

fn parse_signals(logs: &[(String, String)]) -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (_, line) in logs.iter().rev().take(MAX_SIGNAL_LINES) {
        if line.len() > MAX_SIGNAL_LINE_BYTES {
            continue;
        }
        for (prefix, kind) in [
            ("Cannot find package '", "missing-package"),
            ("Cannot find package \"", "missing-package"),
            ("Cannot find module '", "missing-module"),
            ("Cannot find module \"", "missing-module"),
            ("Failed to load plugin '", "plugin-load-failure"),
            ("Failed to load plugin \"", "plugin-load-failure"),
        ] {
            if let Some(package_name) = quoted_after(line, prefix) {
                add_signal(&mut found, package_name, kind);
            }
        }

        let lower = line.to_ascii_lowercase();
        if lower.contains("error")
            || lower.contains("failed")
            || line.trim_start().starts_with("at ")
        {
            let normalized = line.replace('\\', "/");
            let mut remaining = normalized.as_str();
            while let Some(index) = remaining.find("/node_modules/") {
                remaining = &remaining[index + "/node_modules/".len()..];
                if let Some(package_name) = package_from_node_modules(remaining) {
                    add_signal(&mut found, package_name, "error-stack");
                }
                if let Some(next) = remaining.find('/') {
                    remaining = &remaining[next..];
                } else {
                    break;
                }
            }
        }
    }
    found
}

fn quoted_after<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let start = line.find(prefix)? + prefix.len();
    let quote = prefix.chars().last()?;
    let value = line.get(start..)?.split(quote).next()?;
    (!value.is_empty()).then_some(value)
}

fn package_from_node_modules(value: &str) -> Option<&str> {
    let first = value.split('/').next()?;
    if first.starts_with('@') {
        let second = value.split('/').nth(1)?;
        value.get(..first.len().saturating_add(1).saturating_add(second.len()))
    } else {
        Some(first)
    }
}

fn add_signal(found: &mut BTreeMap<String, BTreeSet<String>>, package_name: &str, kind: &str) {
    if plugins::is_valid_package_name(package_name) {
        found
            .entry(package_name.to_string())
            .or_default()
            .insert(kind.to_string());
    }
}

fn transaction_view(journal: &RecoveryJournal) -> RecoveryTransactionView {
    RecoveryTransactionView {
        transaction_id: journal.transaction_id.clone(),
        package_name: journal.package_name.clone(),
        phase: journal.phase,
        signals: journal.signals.clone(),
        market_managed: journal.market_receipt.is_some(),
    }
}

fn recovery_paths(dsh_home: &Path, create: bool) -> Result<RecoveryPaths, String> {
    let tools = plugins::market_tools_dir(dsh_home)?;
    let root = tools.join("recovery");
    if create || root.exists() {
        secure_fs::ensure_private_dir(&root)?;
    }
    let active = root.join("active");
    if create || active.exists() {
        secure_fs::ensure_private_dir(&active)?;
    }
    Ok(RecoveryPaths {
        journal: active.join("journal.json"),
        before: active.join("package.before.json"),
        disabled: active.join("package.disabled.json"),
        active,
    })
}

fn load_journal(dsh_home: &Path) -> Result<Option<RecoveryJournal>, String> {
    let paths = recovery_paths(dsh_home, false)?;
    let Some(bytes) = secure_fs::read_bounded(&paths.journal, JOURNAL_MAX_BYTES as u64)? else {
        return Ok(None);
    };
    let journal: RecoveryJournal = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid plugin recovery journal: {e}"))?;
    if journal.schema_version != JOURNAL_SCHEMA
        || journal.transaction_id.len() != 24
        || !journal
            .transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !plugins::is_valid_package_name(&journal.package_name)
        || journal.signals.len() > 8
    {
        return Err("plugin recovery journal failed validation".to_string());
    }
    Ok(Some(journal))
}

fn write_journal(paths: &RecoveryPaths, journal: &RecoveryJournal) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(journal)
        .map_err(|e| format!("cannot serialize plugin recovery journal: {e}"))?;
    secure_fs::atomic_write(&paths.journal, &bytes, JOURNAL_MAX_BYTES)
}

fn reconcile(dsh_home: &Path) -> Result<Option<RecoveryJournal>, String> {
    let Some(mut journal) = load_journal(dsh_home)? else {
        return Ok(None);
    };
    let paths = recovery_paths(dsh_home, false)?;
    let current = read_current_profile(dsh_home)?;
    let before = secure_fs::read_bounded(&paths.before, PROFILE_MAX_BYTES)?
        .ok_or_else(|| "plugin recovery backup is missing".to_string())?;
    let disabled = secure_fs::read_bounded(&paths.disabled, PROFILE_MAX_BYTES)?
        .ok_or_else(|| "plugin recovery disabled profile is missing".to_string())?;
    match journal.phase {
        RecoveryPhase::Prepared if current == before => {
            cleanup(&paths)?;
            return Ok(None);
        }
        RecoveryPhase::Prepared if current == disabled => {
            journal.phase = RecoveryPhase::DisabledAwaitingBoot;
            write_journal(&paths, &journal)?;
        }
        RecoveryPhase::RollbackPrepared if current == before => {
            cleanup(&paths)?;
            return Ok(None);
        }
        RecoveryPhase::RollbackPrepared if current == disabled => {
            journal.phase = RecoveryPhase::Isolated;
            write_journal(&paths, &journal)?;
        }
        RecoveryPhase::DisabledAwaitingBoot | RecoveryPhase::Isolated if current == disabled => {}
        _ => {
            return Err(
                "web profile changed outside the active plugin recovery transaction".to_string(),
            )
        }
    }
    Ok(Some(journal))
}

fn require_transaction(dsh_home: &Path, transaction_id: &str) -> Result<RecoveryJournal, String> {
    let journal =
        reconcile(dsh_home)?.ok_or_else(|| "no active plugin recovery transaction".to_string())?;
    if journal.transaction_id != transaction_id {
        return Err("plugin recovery transaction id does not match".to_string());
    }
    Ok(journal)
}

fn read_profile_bytes(profile: &Path) -> Result<Vec<u8>, String> {
    secure_fs::read_bounded(&profile.join("package.json"), PROFILE_MAX_BYTES)?
        .ok_or_else(|| "web profile package.json is missing".to_string())
}

fn read_current_profile(dsh_home: &Path) -> Result<Vec<u8>, String> {
    let profile = plugins::profile_dir(dsh_home)?;
    read_profile_bytes(&profile)
}

fn read_recovery_manifest(dsh_home: &Path) -> Result<(PathBuf, Vec<u8>, Value), String> {
    let profile = plugins::profile_dir(dsh_home)?;
    let bytes = read_profile_bytes(&profile)?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("web profile package.json is invalid JSON: {e}"))?;
    if !value.is_object() {
        return Err("web profile package.json must contain an object".to_string());
    }
    Ok((profile, bytes, value))
}

fn serialize_manifest(manifest: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| format!("cannot serialize recovery profile: {e}"))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > PROFILE_MAX_BYTES {
        return Err("web profile package.json exceeds 4 MiB".to_string());
    }
    Ok(bytes)
}

fn write_profile_bytes(dsh_home: &Path, bytes: &[u8]) -> Result<(), String> {
    serde_json::from_slice::<Value>(bytes)
        .map_err(|e| format!("plugin recovery backup is invalid JSON: {e}"))?;
    let profile = plugins::profile_dir(dsh_home)?;
    plugins::write_profile_bytes(&profile, bytes)
}

fn require_current_matches(
    dsh_home: &Path,
    expected_path: &Path,
    label: &str,
) -> Result<(), String> {
    let expected = secure_fs::read_bounded(expected_path, PROFILE_MAX_BYTES)?
        .ok_or_else(|| format!("{label} is missing"))?;
    if read_current_profile(dsh_home)? != expected {
        return Err("web profile changed after plugin recovery; refusing mutation".to_string());
    }
    Ok(())
}

fn validate_backup(bytes: &[u8], package_name: &str) -> Result<(), String> {
    let manifest: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("plugin recovery backup is invalid JSON: {e}"))?;
    if !plugins::profile_dependencies(&manifest)?.contains_key(package_name)
        || !plugins::profile_bundles(&manifest)?.contains(package_name)
    {
        return Err(
            "plugin recovery backup no longer proves an exact active dependency".to_string(),
        );
    }
    Ok(())
}

fn cleanup(paths: &RecoveryPaths) -> Result<(), String> {
    secure_fs::ensure_private_dir(&paths.active)?;
    for path in [&paths.journal, &paths.before, &paths.disabled] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err("plugin recovery file is not a regular file".to_string())
            }
            Ok(_) => fs::remove_file(path)
                .map_err(|e| format!("cannot remove completed plugin recovery file: {e}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect completed plugin recovery file: {error}"
                ))
            }
        }
    }
    fs::remove_dir(&paths.active)
        .map_err(|e| format!("cannot remove completed plugin recovery directory: {e}"))
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn profile(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "dshd-recovery-{name}-{}",
            secure_fs::random_suffix().unwrap()
        ));
        let web = home.join("profiles/web");
        fs::create_dir_all(web.join("node_modules/broken-plugin")).unwrap();
        fs::write(
            web.join("package.json"),
            br#"{
  "dependencies": {
    "broken-plugin": "1.0.0",
    "healthy-plugin": "1.0.0"
  },
  "dsh": {
    "profile": {
      "bundles": [
        "broken-plugin",
        "healthy-plugin"
      ]
    }
  }
}
"#,
        )
        .unwrap();
        home
    }

    fn logs() -> Vec<(String, String)> {
        vec![
            (
                "stderr".to_string(),
                "Error: failed at /tmp/node_modules/broken-plugin/index.js".to_string(),
            ),
            (
                "stderr".to_string(),
                "Cannot find module 'not-active'".to_string(),
            ),
        ]
    }

    fn record_market_receipt(home: &Path) -> PathBuf {
        let path = plugins::market_tools_dir(home)
            .unwrap()
            .join("market-active.json");
        let value = serde_json::json!({
            "plugins": {
                "broken-plugin": {
                    "slug": "broken-plugin",
                    "entryRevision": "revision-1",
                    "packageName": "broken-plugin",
                    "version": "1.0.0",
                    "integrity": "sha512-fixture",
                    "registry": "https://registry.npmjs.org",
                    "tarball": "1.0.0"
                }
            }
        });
        secure_fs::atomic_write(
            &path,
            &serde_json::to_vec_pretty(&value).unwrap(),
            256 * 1024,
        )
        .unwrap();
        path
    }

    #[test]
    fn signals_are_typed_and_intersect_exact_active_dependencies() {
        let home = profile("detect");
        let candidates = detect_candidates(&home, &logs()).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].package_name, "broken-plugin");
        assert_eq!(candidates[0].signals, vec!["error-stack"]);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn begin_removes_only_exact_bundle_and_keeps_backup() {
        let home = profile("begin");
        let transaction = begin(&home, &logs(), true, "broken-plugin").unwrap();
        assert_eq!(transaction.phase, RecoveryPhase::DisabledAwaitingBoot);
        let (_, manifest) = plugins::read_profile_manifest(&home).unwrap();
        let bundles = plugins::profile_bundles(&manifest).unwrap();
        assert!(!bundles.contains("broken-plugin"));
        assert!(bundles.contains("healthy-plugin"));
        assert!(plugins::profile_dependencies(&manifest)
            .unwrap()
            .contains_key("broken-plugin"));
        let paths = recovery_paths(&home, false).unwrap();
        assert!(paths.before.is_file());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn begin_requires_terminal_startup_failure_and_does_not_mutate() {
        let home = profile("non-terminal");
        let package = home.join("profiles/web/package.json");
        let before = fs::read(&package).unwrap();
        assert!(begin(&home, &logs(), false, "broken-plugin").is_err());
        assert_eq!(fs::read(&package).unwrap(), before);
        assert!(!has_active_transaction(&home).unwrap());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn rollback_requires_unchanged_disabled_profile_and_restores_exact_bundle() {
        let home = profile("rollback");
        let original = fs::read(home.join("profiles/web/package.json")).unwrap();
        let transaction = begin(&home, &logs(), true, "broken-plugin").unwrap();
        rollback(&home, &transaction.transaction_id).unwrap();
        assert_eq!(
            fs::read(home.join("profiles/web/package.json")).unwrap(),
            original
        );
        let (_, manifest) = plugins::read_profile_manifest(&home).unwrap();
        assert!(plugins::profile_bundles(&manifest)
            .unwrap()
            .contains("broken-plugin"));
        assert!(!has_active_transaction(&home).unwrap());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn healthy_commit_then_finalize_keeps_plugin_disabled() {
        let home = profile("finalize");
        let receipt_path = record_market_receipt(&home);
        let transaction = begin(&home, &logs(), true, "broken-plugin").unwrap();
        assert!(transaction.market_managed);
        commit_after_ready(&home).unwrap();
        let overview = overview(&home, &logs(), false).unwrap();
        assert_eq!(
            overview.transaction.as_ref().map(|value| value.phase),
            Some(RecoveryPhase::Isolated)
        );
        finalize(&home, &transaction.transaction_id).unwrap();
        let (_, manifest) = plugins::read_profile_manifest(&home).unwrap();
        assert!(!plugins::profile_bundles(&manifest)
            .unwrap()
            .contains("broken-plugin"));
        let active: Value = serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
        assert!(active["plugins"].as_object().unwrap().is_empty());
        assert!(!has_active_transaction(&home).unwrap());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn finalize_requires_a_verified_healthy_boot() {
        let home = profile("finalize-before-ready");
        let transaction = begin(&home, &logs(), true, "broken-plugin").unwrap();
        let error = finalize(&home, &transaction.transaction_id).unwrap_err();
        assert!(error.contains("healthy boot"));
        assert!(has_active_transaction(&home).unwrap());
        rollback(&home, &transaction.transaction_id).unwrap();
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn rollback_refuses_external_profile_drift() {
        let home = profile("drift");
        let transaction = begin(&home, &logs(), true, "broken-plugin").unwrap();
        let package = home.join("profiles/web/package.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&package).unwrap()).unwrap();
        value["external"] = Value::Bool(true);
        fs::write(&package, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert!(rollback(&home, &transaction.transaction_id)
            .unwrap_err()
            .contains("outside"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn prepared_journal_reconciles_both_atomic_crash_boundaries() {
        let home = profile("reconcile");
        let transaction = begin(&home, &logs(), true, "broken-plugin").unwrap();
        let paths = recovery_paths(&home, false).unwrap();
        let mut journal = load_journal(&home).unwrap().unwrap();
        journal.phase = RecoveryPhase::Prepared;
        write_journal(&paths, &journal).unwrap();
        let recovered = reconcile(&home).unwrap().unwrap();
        assert_eq!(recovered.phase, RecoveryPhase::DisabledAwaitingBoot);
        assert_eq!(recovered.transaction_id, transaction.transaction_id);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn prepared_before_profile_write_cleans_without_mutation() {
        let home = profile("prepared-before");
        let transaction = begin(&home, &logs(), true, "broken-plugin").unwrap();
        let paths = recovery_paths(&home, false).unwrap();
        let before = secure_fs::read_bounded(&paths.before, PROFILE_MAX_BYTES)
            .unwrap()
            .unwrap();
        write_profile_bytes(&home, &before).unwrap();
        let mut journal = load_journal(&home).unwrap().unwrap();
        journal.phase = RecoveryPhase::Prepared;
        write_journal(&paths, &journal).unwrap();
        assert!(reconcile(&home).unwrap().is_none());
        assert!(!has_active_transaction(&home).unwrap());
        assert_eq!(journal.transaction_id, transaction.transaction_id);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn overview_cannot_reconcile_an_inflight_prepared_transaction() {
        let home = profile("serialized-prepared");
        let begin_home = home.clone();
        let (prepared_tx, prepared_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let begin_thread = std::thread::spawn(move || {
            let _guard = lock_recovery();
            begin_locked(&begin_home, &logs(), true, "broken-plugin", || {
                prepared_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
            })
        });
        prepared_rx.recv().unwrap();

        let overview_home = home.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (overview_tx, overview_rx) = std::sync::mpsc::channel();
        let overview_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            overview_tx
                .send(overview(&overview_home, &logs(), true))
                .unwrap();
        });
        started_rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(matches!(
            overview_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        let paths = recovery_paths(&home, false).unwrap();
        assert!(paths.journal.is_file());
        assert!(paths.before.is_file());
        assert!(paths.disabled.is_file());

        resume_tx.send(()).unwrap();
        let transaction = begin_thread.join().unwrap().unwrap();
        let observed = overview_rx.recv().unwrap().unwrap();
        overview_thread.join().unwrap();
        assert_eq!(
            observed
                .transaction
                .as_ref()
                .map(|value| value.transaction_id.as_str()),
            Some(transaction.transaction_id.as_str())
        );
        assert!(paths.before.is_file());
        assert!(paths.disabled.is_file());
        rollback(&home, &transaction.transaction_id).unwrap();
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_recovery_directory_is_rejected() {
        use std::os::unix::fs::symlink;
        let home = profile("symlink");
        let tools = home.join(".desktop-tools");
        fs::create_dir_all(&tools).unwrap();
        let outside = home.join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, tools.join("recovery")).unwrap();
        assert!(begin(&home, &logs(), true, "broken-plugin")
            .unwrap_err()
            .contains("real directory"));
        fs::remove_dir_all(home).unwrap();
    }
}
