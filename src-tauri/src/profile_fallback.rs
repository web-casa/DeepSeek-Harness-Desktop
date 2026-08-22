//! Repair the upstream-owned module fallback below `DSH_HOME/profiles/`.
//!
//! Harness deliberately keeps user-installed packages in
//! `profiles/web/node_modules`, while `profiles/node_modules` is an
//! installation-owned fallback farm maintained by
//! `@deepseek-ai/dsh-app-boot::healProfilesModuleFallback`.  It lets the
//! profile-local loader resolve the bundled core packages through Node's
//! normal parent walk.
//!
//! A Windows in-place upgrade can leave the `@deepseek-ai` scope in that
//! fallback farm as a stale directory junction.  The upstream healer cannot
//! repair a *scope directory* junction: its first `mkdirSync(scope)` follows
//! the dangling junction and fails before it reaches the individual package
//! links.  Detect that narrow state before launching Harness, move only that
//! upstream-owned scope to a private, recoverable backup, then invoke the
//! upstream public healer against the bundled runtime.  User plugins,
//! `profiles/web/node_modules`, profile manifests, and presets are never
//! modified.

use crate::paths::RuntimePaths;
use dsh_sidecar::platform::{PlatformChild, SpawnSpec};
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const CORE_SCOPE: &str = "@deepseek-ai";
const WEB_BUNDLE: &str = "@deepseek-ai/dsh-web-app";
const DSH_PACKAGE: &str = "@deepseek-ai/dsh";
const PACKAGE_MANIFEST_MAX_BYTES: u64 = 512 * 1024;
const REPAIR_TIMEOUT: Duration = Duration::from_secs(15);
const REPAIR_POLL: Duration = Duration::from_millis(25);

// This is deliberately an import of the upstream public API, rather than a
// copy of its link-farm algorithm.  `scripts/verify-runtime.ts` exercises the
// exact invocation against the staged runtime, so a Harness upgrade cannot
// silently remove or rename this contract.
const UPSTREAM_HEAL_EVAL: &str = r#"import { healProfilesModuleFallback } from "@deepseek-ai/dsh-app-boot";
healProfilesModuleFallback(process.argv[1], process.argv[2]);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairOutcome {
    NotNeeded,
    Repaired { backup_created: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FallbackHealth {
    NotInitialized,
    Healthy,
    NeedsRepair,
}

/// Repair a pre-existing, broken web-profile fallback if (and only if) the
/// bundled web application's core package links do not resolve to this exact
/// runtime.  A first launch has no profile yet and remains entirely owned by
/// the normal upstream boot path.
pub fn repair_if_needed(
    paths: &RuntimePaths,
    detailed_diagnostics: Option<&crate::diagnostic_mode::DiagnosticMode>,
) -> Result<RepairOutcome, String> {
    match fallback_health(paths)? {
        FallbackHealth::NotInitialized | FallbackHealth::Healthy => Ok(RepairOutcome::NotNeeded),
        FallbackHealth::NeedsRepair => {
            let backup_created = quarantine_core_scope(paths)?.is_some();
            run_upstream_healer(paths, detailed_diagnostics)?;
            match fallback_health(paths)? {
                FallbackHealth::Healthy => Ok(RepairOutcome::Repaired { backup_created }),
                FallbackHealth::NotInitialized | FallbackHealth::NeedsRepair => Err(
                    "the bundled Harness fallback repair completed without restoring every required core package"
                        .to_string(),
                ),
            }
        }
    }
}

fn fallback_health(paths: &RuntimePaths) -> Result<FallbackHealth, String> {
    let profiles = paths.dsh_home.join("profiles");
    if !real_directory_exists(&profiles, "profiles directory")? {
        return Ok(FallbackHealth::NotInitialized);
    }

    let web = profiles.join("web");
    if !real_directory_exists(&web, "web profile directory")? {
        return Ok(FallbackHealth::NotInitialized);
    }
    let web_manifest = web.join("package.json");
    if !regular_file_exists(&web_manifest, "web profile package.json")? {
        return Ok(FallbackHealth::NotInitialized);
    }

    let fallback_root = profiles.join("node_modules");
    if !real_directory_exists(&fallback_root, "profile module fallback directory")? {
        return Ok(FallbackHealth::NeedsRepair);
    }

    for package_name in web_core_packages(paths)? {
        let expected = package_directory(&paths.harness_dir.join("node_modules"), &package_name)?;
        let expected = canonical_real_directory(&expected, "bundled core package")?;
        let actual = package_directory(&fallback_root, &package_name)?;
        match fs::canonicalize(&actual) {
            Ok(actual) if actual == expected => {}
            Ok(_) => return Ok(FallbackHealth::NeedsRepair),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FallbackHealth::NeedsRepair)
            }
            Err(error) => {
                return Err(format!(
                    "cannot resolve profile fallback package {package_name}: {error}"
                ))
            }
        }
    }

    Ok(FallbackHealth::Healthy)
}

/// Parse the trusted bundled `dsh-web-app` manifest rather than carrying a
/// stale hand-written list of UI packages.  Every direct `@deepseek-ai/*`
/// dependency is expected to be reachable from a web profile's fallback
/// layer; this catches a partially stale scope, not only the first package a
/// loader happened to import.
fn web_core_packages(paths: &RuntimePaths) -> Result<Vec<String>, String> {
    let web_manifest = package_directory(&paths.harness_dir.join("node_modules"), WEB_BUNDLE)?
        .join("package.json");
    let bytes = fs::read(&web_manifest).map_err(|error| {
        format!(
            "cannot read bundled web bundle manifest {}: {error}",
            web_manifest.display()
        )
    })?;
    if bytes.len() as u64 > PACKAGE_MANIFEST_MAX_BYTES {
        return Err(format!(
            "bundled web bundle manifest exceeds {PACKAGE_MANIFEST_MAX_BYTES} bytes"
        ));
    }
    let manifest: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("bundled web bundle manifest is invalid JSON: {error}"))?;
    let dependencies = manifest
        .get("dependencies")
        .and_then(Value::as_object)
        .ok_or_else(|| "bundled web bundle manifest has no dependencies object".to_string())?;

    let mut packages = dependencies
        .keys()
        .filter(|name| is_core_package_name(name))
        .cloned()
        .collect::<Vec<_>>();
    packages.push(WEB_BUNDLE.to_string());
    packages.push(DSH_PACKAGE.to_string());
    packages.sort_unstable();
    packages.dedup();
    if packages.len() <= 2 {
        return Err("bundled web bundle manifest exposes no core dependencies".to_string());
    }
    Ok(packages)
}

fn is_core_package_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("@deepseek-ai/") else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains('/')
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn package_directory(node_modules: &Path, package_name: &str) -> Result<PathBuf, String> {
    if !is_core_package_name(package_name) {
        return Err(format!("unreviewed core package name: {package_name}"));
    }
    let name = package_name
        .strip_prefix("@deepseek-ai/")
        .ok_or_else(|| format!("invalid core package name: {package_name}"))?;
    Ok(node_modules.join(CORE_SCOPE).join(name))
}

fn real_directory_exists(path: &Path, label: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if crate::secure_fs::is_symlink_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(format!(
                    "{label} must be a real directory: {}",
                    path.display()
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect {label} {}: {error}",
            path.display()
        )),
    }
}

fn regular_file_exists(path: &Path, label: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if crate::secure_fs::is_symlink_or_reparse(&metadata) || !metadata.is_file() {
                return Err(format!(
                    "{label} must be a regular file: {}",
                    path.display()
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect {label} {}: {error}",
            path.display()
        )),
    }
}

fn canonical_real_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    if !real_directory_exists(path, label)? {
        return Err(format!("{label} is missing: {}", path.display()));
    }
    fs::canonicalize(path).map_err(|error| format!("cannot canonicalize {label}: {error}"))
}

/// Move only a reparse/symlink `@deepseek-ai` scope out of the fallback tree.
/// That is the narrow interrupted-Windows-update state we must repair: an
/// ordinary directory can be reconciled by the upstream healer without
/// copying or retaining an arbitrary user-sized tree. `rename` moves a
/// symlink/junction entry itself rather than walking it, so a broken junction
/// never causes recursive deletion of its old target. The backup is kept
/// below the Desktop-private tools directory for explicit, recoverable
/// forensic inspection.
fn quarantine_core_scope(paths: &RuntimePaths) -> Result<Option<PathBuf>, String> {
    let fallback_root = paths.dsh_home.join("profiles").join("node_modules");
    if !real_directory_exists(&fallback_root, "profile module fallback directory")? {
        return Ok(None);
    }
    let scope = fallback_root.join(CORE_SCOPE);
    match fs::symlink_metadata(&scope) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect core fallback scope {}: {error}",
            scope.display()
        )),
        Ok(metadata) => {
            if !crate::secure_fs::is_symlink_or_reparse(&metadata) {
                return Ok(None);
            }
            // Check each Desktop-owned ancestor rather than calling
            // `create_dir_all` only for the final path: a pre-existing
            // `.desktop-tools` symlink must never redirect this recovery
            // backup outside DSH_HOME.
            let desktop_tools_root = paths.dsh_home.join(".desktop-tools");
            crate::secure_fs::ensure_private_dir(&desktop_tools_root)?;
            let backup_root = desktop_tools_root.join("profile-fallback-backups");
            crate::secure_fs::ensure_private_dir(&backup_root)?;
            let backup = backup_root.join(format!(
                "deepseek-ai-{}",
                crate::secure_fs::random_suffix()?
            ));
            fs::rename(&scope, &backup).map_err(|error| {
                format!(
                    "cannot preserve broken core fallback scope {}: {error}",
                    scope.display()
                )
            })?;
            Ok(Some(backup))
        }
    }
}

fn repair_spawn_spec(paths: &RuntimePaths) -> Result<SpawnSpec, String> {
    if !regular_file_exists(&paths.node, "bundled Node executable")? {
        return Err(format!(
            "bundled Node executable is missing: {}",
            paths.node.display()
        ));
    }
    if !real_directory_exists(&paths.harness_dir, "bundled Harness directory")? {
        return Err(format!(
            "bundled Harness directory is missing: {}",
            paths.harness_dir.display()
        ));
    }
    let anchor = package_directory(&paths.harness_dir.join("node_modules"), DSH_PACKAGE)?
        .join("package.json");
    if !regular_file_exists(&anchor, "bundled dsh package manifest")? {
        return Err(format!(
            "bundled dsh package manifest is missing: {}",
            anchor.display()
        ));
    }
    Ok(SpawnSpec {
        node: paths.node.display().to_string(),
        // Node evaluates the following trusted source as ESM.  This avoids
        // passing a profile path as Node's main entrypoint (which would
        // reintroduce the Windows `\\?\\` main-path normalization edge case).
        script: "--input-type=module".to_string(),
        args: vec![
            "-e".to_string(),
            UPSTREAM_HEAL_EVAL.to_string(),
            anchor.display().to_string(),
            paths.dsh_home.display().to_string(),
        ],
        cwd: paths.harness_dir.display().to_string(),
        env: vec![
            ("DSH_HOME".to_string(), paths.dsh_home.display().to_string()),
            ("DSH_TELEMETRY_DISABLED".to_string(), "1".to_string()),
        ],
    })
}

fn drain<R>(reader: R)
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let _ = dsh_sidecar::for_each_bounded_line(BufReader::new(reader), 8 * 1024, |_| true);
    });
}

/// Drain the repair helper's stderr without retaining any detail unless the
/// user explicitly opted in.  The bounded queue gives the polling parent a
/// chance to persist useful upstream diagnostics while an unexpectedly noisy
/// helper can never block on or grow the Desktop heap.
fn drain_stderr_for_diagnostics<R>(reader: R) -> Receiver<String>
where
    R: Read + Send + 'static,
{
    const QUEUE_LINES: usize = 32;
    let (sender, receiver) = mpsc::sync_channel(QUEUE_LINES);
    std::thread::spawn(move || {
        let _ = dsh_sidecar::for_each_bounded_line(BufReader::new(reader), 8 * 1024, |line| {
            // The repair helper must never stall because diagnostics are
            // slow. A later line is no more authoritative than an earlier
            // one, so losing an excess line is safer than blocking its pipe.
            let _ = sender.try_send(line);
            true
        });
    });
    receiver
}

fn record_repair_stderr(
    receiver: &Receiver<String>,
    mode: &crate::diagnostic_mode::DiagnosticMode,
    dsh_home: &str,
) {
    while let Ok(line) = receiver.try_recv() {
        mode.record_line(
            crate::diagnostic_mode::DetailedLogSource::DesktopError,
            &line,
            dsh_home,
        );
    }
}

/// A child that has exited closes its stderr pipe, so the reader should
/// normally disconnect immediately. Bound the final wait nevertheless: a
/// diagnostic drain may never make startup or recovery wait indefinitely.
fn finish_recording_repair_stderr(
    receiver: &Receiver<String>,
    mode: &crate::diagnostic_mode::DiagnosticMode,
    dsh_home: &str,
) {
    record_repair_stderr(receiver, mode, dsh_home);
    while let Ok(line) = receiver.recv_timeout(Duration::from_millis(250)) {
        mode.record_line(
            crate::diagnostic_mode::DetailedLogSource::DesktopError,
            &line,
            dsh_home,
        );
    }
}

fn run_upstream_healer(
    paths: &RuntimePaths,
    detailed_diagnostics: Option<&crate::diagnostic_mode::DiagnosticMode>,
) -> Result<(), String> {
    let spec = repair_spawn_spec(paths)?;
    let inherited = std::env::vars_os().collect::<Vec<(OsString, OsString)>>();
    let mut child = PlatformChild::spawn(&spec, &inherited)
        .map_err(|error| format!("cannot start bundled profile fallback repair: {error}"))?;
    if let Some(stdout) = child.child.stdout.take() {
        drain(stdout);
    }
    let stderr = match child.child.stderr.take() {
        Some(stderr) if detailed_diagnostics.is_some() => {
            Some(drain_stderr_for_diagnostics(stderr))
        }
        Some(stderr) => {
            drain(stderr);
            None
        }
        None => None,
    };

    let started = Instant::now();
    loop {
        if let (Some(receiver), Some(mode)) = (stderr.as_ref(), detailed_diagnostics) {
            record_repair_stderr(receiver, mode, &paths.dsh_home.to_string_lossy());
        }
        match child.child.try_wait() {
            Ok(Some(status)) => {
                if let (Some(receiver), Some(mode)) = (stderr.as_ref(), detailed_diagnostics) {
                    finish_recording_repair_stderr(
                        receiver,
                        mode,
                        &paths.dsh_home.to_string_lossy(),
                    );
                }
                if status.success() {
                    return Ok(());
                }
                return Err(format!(
                    "bundled profile fallback repair exited with {status}"
                ));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("cannot observe profile fallback repair: {error}")),
        }
        if started.elapsed() >= REPAIR_TIMEOUT {
            child.force();
            return Err(format!(
                "bundled profile fallback repair exceeded {} seconds",
                REPAIR_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(REPAIR_POLL);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn cleanup(paths: &RuntimePaths) {
        let root = paths
            .dsh_home
            .parent()
            .expect("fixture DSH_HOME has a synthetic root");
        fs::remove_dir_all(root).expect("remove complete fixture root");
    }

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshd-profile-fallback-{name}-{}",
            crate::secure_fs::random_suffix().unwrap()
        ))
    }

    fn fixture(name: &str) -> RuntimePaths {
        let root = test_root(name);
        let harness_dir = root.join("runtime/harness");
        let web_bundle = package_directory(&harness_dir.join("node_modules"), WEB_BUNDLE).unwrap();
        let ui_package = package_directory(
            &harness_dir.join("node_modules"),
            "@deepseek-ai/dsh-client-ui-input-trigger",
        )
        .unwrap();
        let dsh_package =
            package_directory(&harness_dir.join("node_modules"), DSH_PACKAGE).unwrap();
        fs::create_dir_all(&web_bundle).unwrap();
        fs::create_dir_all(&ui_package).unwrap();
        fs::create_dir_all(&dsh_package).unwrap();
        fs::write(
            web_bundle.join("package.json"),
            br#"{"dependencies":{"@deepseek-ai/dsh-client-ui-input-trigger":"1.0.0"}}"#,
        )
        .unwrap();
        fs::write(
            dsh_package.join("package.json"),
            br#"{"name":"@deepseek-ai/dsh"}"#,
        )
        .unwrap();
        let web = root.join("home/profiles/web");
        fs::create_dir_all(&web).unwrap();
        fs::write(web.join("package.json"), b"{}\n").unwrap();
        RuntimePaths {
            sidecar: root.join("runtime/sidecar"),
            node: root.join("runtime/node"),
            harness_dir,
            dsh_home: root.join("home"),
        }
    }

    #[cfg(unix)]
    fn link_profile_core_packages(paths: &RuntimePaths) {
        use std::os::unix::fs::symlink;

        let fallback_root = paths.dsh_home.join("profiles/node_modules");
        for package_name in web_core_packages(paths).expect("fixture manifest is valid") {
            let expected =
                package_directory(&paths.harness_dir.join("node_modules"), &package_name)
                    .expect("fixture package name is reviewed");
            let actual = package_directory(&fallback_root, &package_name)
                .expect("fixture package name is reviewed");
            fs::create_dir_all(actual.parent().expect("scoped package has a parent"))
                .expect("create scoped fallback parent");
            symlink(expected, actual).expect("link bundled package into fallback");
        }
    }

    #[test]
    fn missing_existing_profile_fallback_requires_repair() {
        let paths = fixture("missing");
        assert_eq!(
            fallback_health(&paths).unwrap(),
            FallbackHealth::NeedsRepair
        );
        cleanup(&paths);
    }

    #[test]
    fn fresh_home_is_left_to_upstream_initialization() {
        let paths = fixture("fresh");
        fs::remove_dir_all(&paths.dsh_home).unwrap();
        assert_eq!(
            fallback_health(&paths).unwrap(),
            FallbackHealth::NotInitialized
        );
        cleanup(&paths);
    }

    #[cfg(unix)]
    #[test]
    fn healthy_scope_links_to_exact_bundled_package() {
        let paths = fixture("healthy");
        link_profile_core_packages(&paths);
        assert_eq!(fallback_health(&paths).unwrap(), FallbackHealth::Healthy);
        cleanup(&paths);
    }

    #[test]
    fn quarantine_leaves_a_real_scope_for_the_upstream_healer() {
        let paths = fixture("quarantine");
        let fallback_scope = paths.dsh_home.join("profiles/node_modules/@deepseek-ai");
        fs::create_dir_all(&fallback_scope).unwrap();
        fs::write(fallback_scope.join("stale-marker"), b"old").unwrap();
        let user_plugin = paths
            .dsh_home
            .join("profiles/web/node_modules/community-plugin/marker");
        fs::create_dir_all(user_plugin.parent().unwrap()).unwrap();
        fs::write(&user_plugin, b"user").unwrap();

        let backup = quarantine_core_scope(&paths).unwrap();
        assert!(backup.is_none());
        assert_eq!(
            fs::read(fallback_scope.join("stale-marker")).unwrap(),
            b"old"
        );
        assert_eq!(fs::read(user_plugin).unwrap(), b"user");
        cleanup(&paths);
    }

    #[cfg(unix)]
    #[test]
    fn quarantine_moves_a_scope_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let paths = fixture("scope-symlink");
        let fallback_root = paths.dsh_home.join("profiles/node_modules");
        fs::create_dir_all(&fallback_root).unwrap();
        let external = paths.dsh_home.join("external-core-scope");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("marker"), b"outside").unwrap();
        let scope = fallback_root.join(CORE_SCOPE);
        symlink(&external, &scope).unwrap();

        let backup = quarantine_core_scope(&paths)
            .unwrap()
            .expect("scope exists");
        assert!(crate::secure_fs::is_symlink_or_reparse(
            &fs::symlink_metadata(&backup).unwrap()
        ));
        assert_eq!(fs::read(external.join("marker")).unwrap(), b"outside");
        cleanup(&paths);
    }

    #[cfg(windows)]
    #[test]
    fn quarantine_moves_a_scope_junction_without_touching_its_target() {
        let paths = fixture("scope-junction");
        let fallback_root = paths.dsh_home.join("profiles/node_modules");
        fs::create_dir_all(&fallback_root).unwrap();
        let external = paths.dsh_home.join("external-core-scope");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("marker"), b"outside").unwrap();
        let scope = fallback_root.join(CORE_SCOPE);
        let output = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&scope)
            .arg(&external)
            .output()
            .unwrap();
        assert!(output.status.success());

        let backup = quarantine_core_scope(&paths)
            .unwrap()
            .expect("scope exists");
        assert!(crate::secure_fs::is_symlink_or_reparse(
            &fs::symlink_metadata(&backup).unwrap()
        ));
        assert_eq!(fs::read(external.join("marker")).unwrap(), b"outside");
        cleanup(&paths);
    }

    #[test]
    fn repair_spawn_is_fixed_and_has_no_user_shell_fragment() {
        let paths = fixture("spawn-spec");
        fs::write(&paths.node, b"fixture").unwrap();
        let spec = repair_spawn_spec(&paths).unwrap();
        assert_eq!(spec.script, "--input-type=module");
        assert_eq!(spec.args[0], "-e");
        assert_eq!(spec.args[1], UPSTREAM_HEAL_EVAL);
        assert_eq!(spec.args.len(), 4);
        assert_eq!(spec.cwd, paths.harness_dir.display().to_string());
        assert!(spec
            .env
            .iter()
            .any(|(key, value)| key == "DSH_TELEMETRY_DISABLED" && value == "1"));
        cleanup(&paths);
    }

    #[test]
    fn opted_in_repair_stderr_drain_is_bounded_and_never_needs_a_shell() {
        let receiver = drain_stderr_for_diagnostics(Cursor::new(
            b"first upstream repair detail\nsecond upstream repair detail\n".to_vec(),
        ));
        let mut lines = Vec::new();
        while let Ok(line) = receiver.recv_timeout(Duration::from_secs(1)) {
            lines.push(line);
        }
        assert_eq!(
            lines,
            vec![
                "first upstream repair detail".to_string(),
                "second upstream repair detail".to_string(),
            ]
        );
    }
}
