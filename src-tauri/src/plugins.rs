//! In-app plugin installation through the official `dsh plugin` add path and
//! precise scripts-disabled removal through the bundled pnpm entry point.
//!
//! Process-tree guarantees come from reusing dsh-sidecar's `PlatformChild`
//! (unix process group / Windows Job Object): cancel and app-exit always
//! clean the whole node → dsh → pnpm → node-gyp tree. Upstream's init /
//! reconcile logic is untouched — we only make sure it finds a pnpm.

use dsh_sidecar::platform::PlatformChild;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// npm package-name validation, applied SERVER-side before anything spawns:
/// on Windows the name travels through cmd.exe (dsh spawns pnpm with
/// shell:true), so metacharacters must never reach it. Scope + name only.
pub fn is_valid_package_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    let valid_part = |p: &str| {
        !p.is_empty()
            && p.len() <= 214
            && p.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
            })
            // Leading '.'/'_' violate the npm name spec; leading '-' must be
            // rejected too or the single "name" argument would be re-read by
            // pnpm as an `add` flag (-D/-h/--save-dev/--: false-success
            // help, devDependencies split — the name stops being positional).
            && !p.starts_with(['.', '_', '-'])
    };
    let mut parts = name.split('/');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    match (first.starts_with('@'), rest.len()) {
        (true, 1) => valid_part(&first[1..]) && valid_part(rest[0]),
        (false, 0) => valid_part(first),
        _ => false,
    }
}

pub const MAX_SIDELOAD_BYTES: u64 = 64 * 1024 * 1024;

/// Validate a sideload spec: `file:<absolute-path>` with a `.tgz` suffix,
/// no NUL, existing regular file (not a symlink), size <= 64 MiB. This is
/// only a pre-check; execution always copies to a safe tools-owned path.
#[allow(dead_code)] // kept as a pure validator for tests and future callers
pub fn is_valid_sideload_spec(spec: &str) -> bool {
    let Some(path) = spec.strip_prefix("file:") else {
        return false;
    };
    validate_sideload_path(Path::new(path)).is_ok()
}

fn validate_sideload_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("sideload path must be absolute".to_string());
    }
    if path.as_os_str().to_string_lossy().contains('\0') {
        return Err("sideload path must not contain NUL".to_string());
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err("sideload path must name a file".to_string());
    };
    if !name.to_ascii_lowercase().ends_with(".tgz") {
        return Err("sideload file must end with .tgz".to_string());
    }
    let file = crate::secure_fs::open_regular_read(path)
        .map_err(|e| format!("cannot securely open sideload file: {e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("cannot inspect sideload file: {e}"))?;
    if meta.len() > MAX_SIDELOAD_BYTES {
        return Err("sideload file exceeds 64 MiB".to_string());
    }
    Ok(())
}

/// Copy a user-selected .tgz into `<dsh_home>/.desktop-tools/sideload/`
/// under an application-generated ASCII filename. The returned path is safe
/// to pass to the plugin runner: it contains no user-controlled shell
/// metacharacters and sits in a directory we refuse to create through a
/// symlink.
/// Fail-closed shell-safety check for the FINAL spec string that will be
/// forwarded to upstream `spawnSync("pnpm", { shell: win32 })`. Paths with
/// spaces or cmd metacharacters are rejected instead of ever being parsed by
/// the shell.
#[cfg_attr(not(windows), allow(dead_code))] // Windows-only safety gate
pub fn is_shell_safe_spec(spec: &str) -> bool {
    !spec.is_empty()
        && spec.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b':' | b'/' | b'\\' | b'.' | b'-' | b'_')
        })
}

pub fn stage_sideload(dsh_home: &Path, src: &Path) -> Result<PathBuf, String> {
    validate_sideload_path(src)?;
    let input = crate::secure_fs::open_regular_read(src)
        .map_err(|e| format!("cannot securely open sideload file: {e}"))?;
    let source_size = input
        .metadata()
        .map_err(|e| format!("cannot inspect open sideload file: {e}"))?
        .len();
    if source_size > MAX_SIDELOAD_BYTES {
        return Err("sideload file exceeds 64 MiB".to_string());
    }

    let tools = market_tools_dir(dsh_home)?;
    let dir = tools.join("sideload");
    crate::secure_fs::ensure_private_dir(&dir)?;
    let dest = dir.join(format!(
        "sideload-{}.tgz",
        crate::secure_fs::random_suffix()?
    ));
    let result = (|| {
        let mut output = crate::secure_fs::create_private_new(&dest)?;
        let mut bounded = input.take(MAX_SIDELOAD_BYTES.saturating_add(1));
        let copied = std::io::copy(&mut bounded, &mut output)
            .map_err(|e| format!("cannot stage sideload file: {e}"))?;
        if copied > MAX_SIDELOAD_BYTES {
            return Err("sideload file changed beyond 64 MiB while staging".to_string());
        }
        output
            .sync_all()
            .map_err(|e| format!("cannot sync staged sideload file: {e}"))?;
        Ok(dest.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&dest);
    }
    result
}

fn posix_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Pure shim-text generation (unit-tested).
pub fn pnpm_shim_script(node: &str, pnpm_cjs: &str) -> String {
    format!(
        "#!/bin/sh\nexec {} {} \"$@\"\n",
        posix_shell_quote(node),
        posix_shell_quote(pnpm_cjs)
    )
}

pub fn pnpm_shim_cmd(node: &str, pnpm_cjs: &str) -> String {
    // Percent expansion happens even inside quotes in a batch file. Doubling
    // it preserves literal percent signs in valid Windows paths; delayed
    // expansion is disabled so a literal `!` is preserved too.
    let node = node.replace('%', "%%");
    let pnpm_cjs = pnpm_cjs.replace('%', "%%");
    format!("@echo off\nsetlocal DisableDelayedExpansion\n\"{node}\" \"{pnpm_cjs}\" %*\n")
}

/// Ensure `<dsh_home>/.desktop-tools/` holds the pnpm shims; returns the
/// directory to prepend to PATH for the plugin child. Both shims are written
/// on every platform: cmd.exe resolves `pnpm` via PATHEXT to `pnpm.cmd`
/// (extensionless files are never executed there, so the unix script is
/// inert), while the `.cmd` variant also covers git-bash on Windows.
pub fn ensure_pnpm_shim(dsh_home: &Path, node: &Path, pnpm_cjs: &Path) -> Result<PathBuf, String> {
    let dir = market_tools_dir(dsh_home)?;
    let node_s = node.display().to_string();
    let cjs_s = pnpm_cjs.display().to_string();
    let unix = dir.join("pnpm");
    crate::secure_fs::atomic_write(
        &unix,
        pnpm_shim_script(&node_s, &cjs_s).as_bytes(),
        64 * 1024,
    )?;
    let win = dir.join("pnpm.cmd");
    crate::secure_fs::atomic_write(&win, pnpm_shim_cmd(&node_s, &cjs_s).as_bytes(), 64 * 1024)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Set executable permission through a no-follow file handle, not the
        // pathname, so a concurrent leaf swap cannot redirect chmod.
        crate::secure_fs::open_regular_read(&unix)?
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

// ---------------------------------------------------------------------------
// cordis.run market installation state
// ---------------------------------------------------------------------------
//
// The upstream dsh plugin add command intentionally reconciles every
// installed dsh.bundle into dsh.profile.bundles. That is correct for its CLI,
// but it would violate the market contract's pending-activation boundary.
// Market installs therefore invoke the bundled pnpm directly (with scripts
// disabled) and maintain this narrowly scoped, Desktop-owned pending record.

const MARKET_PENDING_FILE: &str = "market-pending.json";
const MARKET_ACTIVE_FILE: &str = "market-active.json";
const MARKET_PENDING_MAX_BYTES: u64 = 256 * 1024;
const PROFILE_LOCK_MAX_BYTES: u64 = 4 * 1024 * 1024;
const PROFILE_MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;
const INSTALLED_MANIFEST_MAX_BYTES: u64 = 256 * 1024;
const BUNDLE_PATCH_MAX_BYTES: u64 = 4 * 1024 * 1024;
const BUNDLE_PATCH_PATH_MAX_BYTES: usize = 1024;
const PROFILE_NPMRC_MAX_BYTES: u64 = 64 * 1024;
const PNPM_MODULES_STATE_MAX_BYTES: u64 = 1024 * 1024;
const PNPM_STORE_PATH_MAX_BYTES: usize = 4096;
const MAX_INSTALLED_PLUGINS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketPendingPlugin {
    pub slug: String,
    pub entry_revision: String,
    pub package_name: String,
    pub version: String,
    pub integrity: String,
    pub registry: String,
    pub tarball: String,
}

impl From<&crate::market::MarketInstallCandidate> for MarketPendingPlugin {
    fn from(candidate: &crate::market::MarketInstallCandidate) -> Self {
        MarketPendingPlugin {
            slug: candidate.slug.clone(),
            entry_revision: candidate.entry_revision.clone(),
            package_name: candidate.package_name.clone(),
            version: candidate.version.clone(),
            integrity: candidate.integrity.clone(),
            registry: candidate.registry.clone(),
            tarball: candidate.tarball.clone(),
        }
    }
}

impl MarketPendingPlugin {
    pub(crate) fn matches(&self, candidate: &crate::market::MarketInstallCandidate) -> bool {
        self.slug == candidate.slug
            && self.entry_revision == candidate.entry_revision
            && self.package_name == candidate.package_name
            && self.version == candidate.version
            && self.integrity == candidate.integrity
            && self.registry == candidate.registry
            && self.tarball == candidate.tarball
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MarketPendingFile {
    #[serde(default)]
    plugins: BTreeMap<String, MarketPendingPlugin>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MarketActiveFile {
    #[serde(default)]
    plugins: BTreeMap<String, MarketPendingPlugin>,
}

/// A bootstrap-facing installed-plugin row. pending is the only state that
/// may be activated through the Desktop IPC; generic upstream-installed
/// dependencies stay displayable but never gain a synthetic Activate button.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub name: String,
    pub version: String,
    pub state: String,
    pub slug: Option<String>,
    pub entry_revision: Option<String>,
}

fn checked_real_dir(path: &Path, label: &str) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| format!("cannot inspect {label}: {e}"))?;
    if crate::secure_fs::is_symlink_or_reparse(&metadata) {
        return Err(format!(
            "{label} must not be a symbolic link or reparse point"
        ));
    }
    if !metadata.is_dir() {
        return Err(format!("{label} is not a directory"));
    }
    Ok(())
}

pub(crate) fn market_tools_dir(dsh_home: &Path) -> Result<PathBuf, String> {
    checked_real_dir(dsh_home, "DSH_HOME")?;
    let tools = dsh_home.join(".desktop-tools");
    crate::secure_fs::ensure_private_dir(&tools)
        .map_err(|e| format!("cannot prepare .desktop-tools: {e}"))?;
    Ok(tools)
}

fn pending_path(dsh_home: &Path) -> Result<PathBuf, String> {
    Ok(market_tools_dir(dsh_home)?.join(MARKET_PENDING_FILE))
}

fn load_pending(dsh_home: &Path) -> Result<MarketPendingFile, String> {
    let path = pending_path(dsh_home)?;
    let Some(bytes) = crate::secure_fs::read_bounded(&path, MARKET_PENDING_MAX_BYTES)? else {
        return Ok(MarketPendingFile::default());
    };
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid market pending state: {e}"))
}

fn active_path(dsh_home: &Path) -> Result<PathBuf, String> {
    Ok(market_tools_dir(dsh_home)?.join(MARKET_ACTIVE_FILE))
}

fn load_active(dsh_home: &Path) -> Result<MarketActiveFile, String> {
    let path = active_path(dsh_home)?;
    let Some(bytes) = crate::secure_fs::read_bounded(&path, MARKET_PENDING_MAX_BYTES)? else {
        return Ok(MarketActiveFile::default());
    };
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid active market receipt: {e}"))
}

fn write_active(dsh_home: &Path, active: &MarketActiveFile) -> Result<(), String> {
    let path = active_path(dsh_home)?;
    let bytes = serde_json::to_vec_pretty(active)
        .map_err(|e| format!("cannot serialize active market receipt: {e}"))?;
    crate::secure_fs::atomic_write(&path, &bytes, MARKET_PENDING_MAX_BYTES as usize)
        .map_err(|e| format!("cannot write active market receipt: {e}"))
}

/// Persist exact live-reviewed provenance before any caller enables a market
/// bundle. Writing this first makes both normal activation and recovery
/// rollback crash-safe: an inactive orphan receipt is pruned, while an enabled
/// bundle never exists without the receipt needed for future gated recovery.
pub(crate) fn record_active_market_receipt(
    dsh_home: &Path,
    candidate: &crate::market::MarketInstallCandidate,
) -> Result<(), String> {
    let mut active = load_active(dsh_home)?;
    active.plugins.insert(
        candidate.package_name.clone(),
        MarketPendingPlugin::from(candidate),
    );
    write_active(dsh_home, &active)
}

fn receipt_matches_active_profile(
    manifest: &Value,
    package_name: &str,
    receipt: &MarketPendingPlugin,
) -> Result<bool, String> {
    Ok(receipt.package_name == package_name
        && profile_dependencies(manifest)?
            .get(package_name)
            .and_then(Value::as_str)
            == Some(receipt.tarball.as_str())
        && profile_bundles(manifest)?.contains(package_name))
}

pub(crate) fn active_market_receipt(
    dsh_home: &Path,
    package_name: &str,
) -> Result<Option<MarketPendingPlugin>, String> {
    let active = load_active(dsh_home)?;
    let Some(receipt) = active.plugins.get(package_name) else {
        return Ok(None);
    };
    let (_, manifest) = read_profile_manifest(dsh_home)?;
    Ok(receipt_matches_active_profile(&manifest, package_name, receipt)?.then(|| receipt.clone()))
}

/// Reconcile durable market provenance after the official plugin CLI may have
/// removed, replaced, or reactivated dependencies. Only a currently active
/// bundle whose saved source is the exact reviewed tarball retains authority
/// to request a market-gated recovery rollback.
pub(crate) fn reconcile_active_market_receipts(dsh_home: &Path) -> Result<(), String> {
    let mut active = load_active(dsh_home)?;
    if active.plugins.is_empty() {
        return Ok(());
    }
    let (_, manifest) = read_profile_manifest(dsh_home)?;
    let dependencies = profile_dependencies(&manifest)?;
    let bundles = profile_bundles(&manifest)?;
    let before = active.plugins.len();
    active.plugins.retain(|package_name, receipt| {
        receipt.package_name == *package_name
            && dependencies.get(package_name).and_then(Value::as_str)
                == Some(receipt.tarball.as_str())
            && bundles.contains(package_name.as_str())
    });
    if active.plugins.len() != before {
        write_active(dsh_home, &active)?;
    }
    Ok(())
}

fn reconcile_pending_market_receipts(dsh_home: &Path) -> Result<(), String> {
    let mut pending = load_pending(dsh_home)?;
    if pending.plugins.is_empty() {
        return Ok(());
    }
    // Remember every structurally credible pending name before pruning stale
    // source/version receipts. If an old global reconciliation activated one
    // and its dependency was then replaced, dropping only the stale marker
    // would leave unreviewed content enabled. Stale pending authority must be
    // removed from both the marker file and the active bundle layer.
    let pending_names: HashSet<String> = pending
        .plugins
        .iter()
        .filter(|(name, receipt)| is_valid_package_name(name) && receipt.package_name == **name)
        .map(|(name, _)| name.clone())
        .collect();
    let active = load_active(dsh_home)?;
    let (profile, mut manifest) = read_profile_manifest(dsh_home)?;
    let dependencies = profile_dependencies(&manifest)?;
    let before = pending.plugins.len();
    let mut retained = BTreeMap::new();
    for (package_name, receipt) in &pending.plugins {
        let source_matches = is_valid_package_name(package_name)
            && receipt.package_name == *package_name
            && dependencies.get(package_name).and_then(Value::as_str)
                == Some(receipt.tarball.as_str());
        let installed_version = read_installed_plugin_version(&profile, package_name)
            .ok()
            .flatten();
        if source_matches && installed_version.as_deref() == Some(receipt.version.as_str()) {
            retained.insert(package_name.clone(), receipt.clone());
        }
    }
    pending.plugins = retained;

    // A generic upstream `dsh plugin` mutation reconciles every installed
    // bundle, not only its target. If a previous Desktop build or an
    // interrupted operation let that reconciliation add a still-pending
    // market package, restore the explicit-Activate boundary before exposing
    // the profile again. Only receipts whose exact tarball and installed
    // version still match retain this authority.
    let bundles = profile_bundles_mut(&mut manifest)?;
    for bundle in bundles.iter() {
        if bundle.as_str().is_none() {
            return Err("web profile bundles contains a non-string value".to_string());
        }
    }
    // Activate persists an exact active receipt BEFORE writing bundles and
    // removes the pending marker afterwards. A crash between those final two
    // writes is an explicitly authorized activation, not the old global-
    // reconcile bug: preserve its bundle and complete marker cleanup. A
    // pending bundle without the matching active receipt was never approved
    // and must be removed before Harness starts.
    let explicitly_committed: HashSet<String> = bundles
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| {
            pending
                .plugins
                .get(*name)
                .is_some_and(|receipt| active.plugins.get(*name) == Some(receipt))
        })
        .map(str::to_string)
        .collect();
    let bundle_count = bundles.len();
    bundles.retain(|bundle| {
        bundle
            .as_str()
            .is_none_or(|name| !pending_names.contains(name) || explicitly_committed.contains(name))
    });
    pending
        .plugins
        .retain(|name, _| !explicitly_committed.contains(name));
    if bundles.len() != bundle_count {
        write_profile_manifest(&profile, &manifest)?;
    }
    if pending.plugins.len() != before {
        write_pending(dsh_home, &pending)?;
    }
    Ok(())
}

pub(crate) fn reconcile_market_receipts(dsh_home: &Path) -> Result<(), String> {
    reconcile_pending_market_receipts(dsh_home)?;
    reconcile_active_market_receipts(dsh_home)
}

/// Generic `dsh plugin add` reconciles every dependency and can therefore
/// activate an unrelated market package that is still awaiting explicit
/// confirmation. Keep generic/sideload additions unavailable until all
/// pending market decisions have been resolved; direct market installs do
/// not use the upstream reconciliation path and are unaffected.
pub(crate) fn ensure_no_pending_market_plugins(dsh_home: &Path) -> Result<(), String> {
    ensure_generic_plugin_layout(dsh_home)?;
    reconcile_market_receipts(dsh_home)?;
    if load_pending(dsh_home)?.plugins.is_empty() {
        Ok(())
    } else {
        Err(
            "a market plugin is awaiting explicit activation; activate or uninstall it before adding another plugin"
                .to_string(),
        )
    }
}

/// The upstream add path initializes a missing profile itself, so it cannot
/// use `profile_dir` (which requires an existing manifest). Validate every
/// hierarchy component that already exists before handing paths to Node;
/// missing components remain available for the official initializer.
fn ensure_generic_plugin_layout(dsh_home: &Path) -> Result<(), String> {
    checked_real_dir(dsh_home, "DSH_HOME")?;
    let profiles = dsh_home.join("profiles");
    match std::fs::symlink_metadata(&profiles) {
        Ok(_) => checked_real_dir(&profiles, "profiles directory")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot inspect profiles directory: {error}")),
    }
    let profile = profiles.join("web");
    match std::fs::symlink_metadata(&profile) {
        Ok(_) => checked_real_dir(&profile, "web profile directory")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot inspect web profile directory: {error}")),
    }
    let manifest = profile.join("package.json");
    match std::fs::symlink_metadata(&manifest) {
        Ok(metadata)
            if crate::secure_fs::is_symlink_or_reparse(&metadata) || !metadata.is_file() =>
        {
            return Err("web profile package.json must be a regular file".to_string());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect web profile package.json: {error}")),
    }
    let node_modules = profile.join("node_modules");
    match std::fs::symlink_metadata(&node_modules) {
        Ok(_) => checked_real_dir(&node_modules, "plugin profile node_modules")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect plugin profile node_modules: {error}"
            ));
        }
    }
    Ok(())
}

pub(crate) fn remove_active_market_receipt(
    dsh_home: &Path,
    package_name: &str,
) -> Result<(), String> {
    let mut active = load_active(dsh_home)?;
    if active.plugins.remove(package_name).is_some() {
        write_active(dsh_home, &active)?;
    }
    Ok(())
}

fn remove_pending_market_receipt(dsh_home: &Path, package_name: &str) -> Result<(), String> {
    let mut pending = load_pending(dsh_home)?;
    if pending.plugins.remove(package_name).is_some() {
        write_pending(dsh_home, &pending)?;
    }
    Ok(())
}

/// Replace a same-directory temporary file without ever following the
/// destination as a symlink. `std::fs::rename` replaces an existing file on
/// Unix but intentionally does not do so on Windows, where it would make the
/// second pending-state write (the Activate path) fail. MoveFileExW supplies
/// the corresponding replace-existing operation there.
fn replace_existing_file(temp: &Path, destination: &Path, label: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::iter::once;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let temp_wide: Vec<u16> = temp.as_os_str().encode_wide().chain(once(0)).collect();
        let destination_wide: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(once(0))
            .collect();
        // SAFETY: both buffers are NUL-terminated UTF-16 paths and remain
        // alive throughout the synchronous Win32 call.
        if unsafe {
            MoveFileExW(
                temp_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(format!(
                "cannot replace {label}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(temp, destination).map_err(|e| format!("cannot replace {label}: {e}"))
    }
}

fn write_pending(dsh_home: &Path, pending: &MarketPendingFile) -> Result<(), String> {
    let path = pending_path(dsh_home)?;
    let mut encoded = serde_json::to_vec_pretty(pending)
        .map_err(|e| format!("cannot serialize market pending state: {e}"))?;
    encoded.push(b'\n');
    crate::secure_fs::atomic_write(&path, &encoded, MARKET_PENDING_MAX_BYTES as usize)
        .map_err(|e| format!("cannot write market pending state: {e}"))
}

pub(crate) fn profile_dir(dsh_home: &Path) -> Result<PathBuf, String> {
    checked_real_dir(dsh_home, "DSH_HOME")?;
    let profiles = dsh_home.join("profiles");
    checked_real_dir(&profiles, "profiles directory")?;
    let web = profiles.join("web");
    checked_real_dir(&web, "web profile directory")?;
    let manifest = web.join("package.json");
    let metadata = std::fs::symlink_metadata(&manifest)
        .map_err(|e| format!("cannot inspect web profile package.json: {e}"))?;
    if crate::secure_fs::is_symlink_or_reparse(&metadata) || !metadata.is_file() {
        return Err("web profile package.json must be a regular file".to_string());
    }
    Ok(web)
}

/// Resolve the already-initialized web profile for a direct market pnpm
/// operation. It deliberately does not create a profile: Harness owns the
/// initialization template and must have started successfully first.
pub fn market_profile_dir(dsh_home: &Path) -> Result<PathBuf, String> {
    profile_dir(dsh_home)
}

/// Recover the store root already bound to this profile's `node_modules`.
/// pnpm refuses to operate when a command selects a different store, so a
/// newly introduced Desktop-private store would break every profile created
/// by an earlier release. pnpm 11 records the versioned directory (…/v11)
/// in `.modules.yaml`; the CLI expects its parent as `--store-dir`.
pub(crate) fn pnpm_store_base(
    profile: &Path,
    bundled_pnpm_major: u64,
) -> Result<Option<PathBuf>, String> {
    if bundled_pnpm_major == 0 {
        return Err("bundled pnpm has an invalid major version".to_string());
    }
    let node_modules = profile.join("node_modules");
    match std::fs::symlink_metadata(&node_modules) {
        Ok(_) => checked_real_dir(&node_modules, "plugin profile node_modules")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect plugin profile node_modules: {error}"
            ))
        }
    }
    let modules_state = node_modules.join(".modules.yaml");
    let Some(bytes) = crate::secure_fs::read_bounded(&modules_state, PNPM_MODULES_STATE_MAX_BYTES)?
    else {
        return Ok(None);
    };

    // pnpm 11 writes JSON (which is valid YAML). Retain a narrow fallback for
    // profiles created by older supported runtimes that wrote a scalar YAML
    // field instead.
    let recorded = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value
            .get("storeDir")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "pnpm modules state has no string storeDir".to_string())?,
        Err(_) => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| format!("pnpm modules state is not valid UTF-8: {error}"))?;
            text.lines()
                .find_map(|line| line.strip_prefix("storeDir:").map(yaml_scalar))
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| "pnpm modules state has no storeDir".to_string())?
        }
    };
    if recorded.len() > PNPM_STORE_PATH_MAX_BYTES || recorded.chars().any(char::is_control) {
        return Err("pnpm modules state has an invalid storeDir".to_string());
    }
    let versioned = PathBuf::from(&recorded);
    if !versioned.is_absolute() {
        return Err("pnpm modules state storeDir must be absolute".to_string());
    }
    let expected_layout = format!("v{bundled_pnpm_major}");
    if versioned.file_name().and_then(|part| part.to_str()) != Some(expected_layout.as_str()) {
        return Err(format!(
            "plugin profile uses an incompatible pnpm store layout ({recorded}); expected {expected_layout}"
        ));
    }
    let base = versioned
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "pnpm modules state storeDir has no usable parent".to_string())?;
    Ok(Some(base.to_path_buf()))
}

/// Return the store already bound to an initialized generic-plugin profile,
/// while allowing the official add path to initialize a genuinely missing
/// profile. Every existing hierarchy component was validated by the caller's
/// generic-layout gate before this helper is used.
pub(crate) fn generic_profile_store_base(
    dsh_home: &Path,
    bundled_pnpm_major: u64,
) -> Result<Option<PathBuf>, String> {
    ensure_generic_plugin_layout(dsh_home)?;
    let manifest = dsh_home.join("profiles/web/package.json");
    match std::fs::symlink_metadata(&manifest) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot inspect web profile package.json: {error}")),
        Ok(_) => {
            let profile = profile_dir(dsh_home)?;
            pnpm_store_base(&profile, bundled_pnpm_major)
        }
    }
}

/// pnpm applies `pnpm.patchedDependencies` after validating the registry
/// tarball. That would let the extracted package differ from the Cordis
/// integrity while the lockfile still reports the reviewed SHA-512, so the
/// strict market path refuses this local content-transform feature.
pub fn ensure_market_install_config(dsh_home: &Path) -> Result<(), String> {
    let (profile, manifest) = read_profile_manifest(dsh_home)?;
    if let Some(npmrc) =
        crate::secure_fs::read_bounded(&profile.join(".npmrc"), PROFILE_NPMRC_MAX_BYTES)?
    {
        if npmrc.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return Err(
                "market installation requires the web profile .npmrc to be absent or empty; local pnpm configuration could redirect the reviewed install"
                    .to_string(),
            );
        }
    }
    let Some(pnpm) = manifest.get("pnpm") else {
        return Ok(());
    };
    let pnpm = pnpm
        .as_object()
        .ok_or_else(|| "web profile package.json has an invalid pnpm configuration".to_string())?;
    if let Some(patched) = pnpm.get("patchedDependencies") {
        let patched = patched.as_object().ok_or_else(|| {
            "web profile package.json has invalid pnpm.patchedDependencies".to_string()
        })?;
        if !patched.is_empty() {
            return Err(
                "market installation requires pnpm.patchedDependencies to be empty so reviewed integrity cannot be transformed"
                    .to_string(),
            );
        }
    }
    Ok(())
}

pub(crate) fn read_profile_manifest(dsh_home: &Path) -> Result<(PathBuf, Value), String> {
    let profile = profile_dir(dsh_home)?;
    let manifest = profile.join("package.json");
    let bytes = crate::secure_fs::read_bounded(&manifest, PROFILE_MANIFEST_MAX_BYTES)?
        .ok_or_else(|| "web profile package.json is missing".to_string())?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("web profile package.json is invalid JSON: {e}"))?;
    if !value.is_object() {
        return Err("web profile package.json must contain an object".to_string());
    }
    Ok((profile, value))
}

pub(crate) fn write_profile_manifest(profile: &Path, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("cannot serialize web profile package.json: {e}"))?;
    bytes.push(b'\n');
    write_profile_bytes(profile, &bytes)
}

pub(crate) fn write_profile_bytes(profile: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 > PROFILE_MANIFEST_MAX_BYTES {
        return Err("web profile package.json exceeds 4 MiB".to_string());
    }
    let path = profile.join("package.json");
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("cannot inspect web profile package.json: {e}"))?;
    if crate::secure_fs::is_symlink_or_reparse(&metadata) || !metadata.is_file() {
        return Err("web profile package.json must be a regular file".to_string());
    }
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let temp = path.with_file_name(format!(
        ".profile-package-{}-{}.tmp",
        std::process::id(),
        millis
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| format!("cannot create web profile package.json temp file: {e}"))?;
        file.write_all(bytes)
            .map_err(|e| format!("cannot write web profile package.json: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("cannot sync web profile package.json: {e}"))?;
        #[cfg(unix)]
        file.set_permissions(metadata.permissions())
            .map_err(|e| format!("cannot preserve web profile package.json permissions: {e}"))?;
        // Windows replacement must not depend on delete-sharing behavior for
        // an open temp handle. On Unix this also makes the durability boundary
        // and publication order explicit.
        drop(file);
        replace_existing_file(&temp, &path, "web profile package.json")
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub(crate) fn profile_bundles_mut(value: &mut Value) -> Result<&mut Vec<Value>, String> {
    value
        .get_mut("dsh")
        .and_then(Value::as_object_mut)
        .and_then(|dsh| dsh.get_mut("profile"))
        .and_then(Value::as_object_mut)
        .and_then(|profile| profile.get_mut("bundles"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "web profile package.json has no dsh.profile.bundles array".to_string())
}

pub(crate) fn profile_bundles(value: &Value) -> Result<HashSet<&str>, String> {
    let values = value
        .get("dsh")
        .and_then(Value::as_object)
        .and_then(|dsh| dsh.get("profile"))
        .and_then(Value::as_object)
        .and_then(|profile| profile.get("bundles"))
        .and_then(Value::as_array)
        .ok_or_else(|| "web profile package.json has no dsh.profile.bundles array".to_string())?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "web profile bundles contains a non-string value".to_string())
        })
        .collect()
}

pub(crate) fn profile_dependencies(
    value: &Value,
) -> Result<&serde_json::Map<String, Value>, String> {
    value
        .get("dependencies")
        .and_then(Value::as_object)
        .ok_or_else(|| "web profile package.json has no dependencies object".to_string())
}

/// Before direct pnpm installation, remove an already active catalog package
/// from the bundle list. This guarantees a replacement cannot take effect
/// while its new artifact is being inspected and verified.
pub fn pre_disable_market_plugin(
    dsh_home: &Path,
    candidate: &crate::market::MarketInstallCandidate,
) -> Result<(), String> {
    pre_disable_plugin(dsh_home, &candidate.package_name)?;
    remove_active_market_receipt(dsh_home, &candidate.package_name)?;
    remove_pending_market_receipt(dsh_home, &candidate.package_name)
}

/// Remove exactly one dependency from the active bundle layer before pnpm
/// mutates it. This is intentionally narrower than upstream's global
/// reconciliation: uninstalling package B must never activate unrelated
/// pending market package A.
pub fn pre_disable_plugin(dsh_home: &Path, package_name: &str) -> Result<(), String> {
    if !is_valid_package_name(package_name) {
        return Err("cannot pre-disable an invalid plugin package name".to_string());
    }
    let (profile, mut manifest) = read_profile_manifest(dsh_home)?;
    pre_disable_profile_bundle(&profile, &mut manifest, package_name)
}

/// Uninstall is exposed over IPC, so it must not let a caller disable an
/// in-box bundle merely by naming it. Require a real, non-empty dependency
/// entry before applying the exact pre-disable mutation; a missing installed
/// tree is still removable so broken installations remain recoverable.
pub fn pre_disable_installed_plugin(dsh_home: &Path, package_name: &str) -> Result<(), String> {
    if !is_valid_package_name(package_name) {
        return Err("cannot pre-disable an invalid plugin package name".to_string());
    }
    let (profile, mut manifest) = read_profile_manifest(dsh_home)?;
    let installed = profile_dependencies(&manifest)?
        .get(package_name)
        .and_then(Value::as_str)
        .is_some_and(|spec| !spec.is_empty());
    if !installed {
        return Err(format!(
            "plugin {package_name} is not an installed web-profile dependency"
        ));
    }
    pre_disable_profile_bundle(&profile, &mut manifest, package_name)
}

fn pre_disable_profile_bundle(
    profile: &Path,
    manifest: &mut Value,
    package_name: &str,
) -> Result<(), String> {
    let bundles = profile_bundles_mut(manifest)?;
    for bundle in bundles.iter() {
        if bundle.as_str().is_none() {
            return Err("web profile bundles contains a non-string value".to_string());
        }
    }
    let before = bundles.len();
    bundles.retain(|bundle| bundle.as_str() != Some(package_name));
    if bundles.len() != before {
        write_profile_manifest(profile, manifest)?;
    }
    Ok(())
}

fn lockfile_package_key(line: &str) -> Option<&str> {
    let raw = line.strip_prefix("  ")?;
    if raw.starts_with("  ") || !raw.ends_with(':') {
        return None;
    }
    let raw = raw.strip_suffix(':')?;
    if raw.len() >= 2
        && ((raw.starts_with('\'') && raw.ends_with('\''))
            || (raw.starts_with('"') && raw.ends_with('"')))
    {
        return Some(&raw[1..raw.len() - 1]);
    }
    Some(raw)
}

/// pnpm writes the package key as `name@version` when no peer context is
/// involved, but appends one or more parenthesized peer contexts when it is
/// (for example `name@1.2.3(react@18.3.1)`).  The peer context is not part of
/// the reviewed package identity, so accept it only after an exact
/// `name@version` prefix and only when its delimiters are well-formed.  Do not
/// use a loose prefix match here: `name@1.2.30(...)` must never satisfy a
/// candidate for `name@1.2.3`.
fn lockfile_package_key_matches(key: &str, expected: &str) -> bool {
    if key == expected {
        return true;
    }
    let Some(suffix) = key.strip_prefix(expected) else {
        return false;
    };
    if !suffix.starts_with('(') {
        return false;
    }

    let mut depth = 0_usize;
    let mut group_has_content = false;
    for character in suffix.chars() {
        if character.is_control() || character.is_whitespace() {
            return false;
        }
        match character {
            '(' => {
                if depth == 0 {
                    group_has_content = false;
                }
                depth += 1;
            }
            ')' => {
                if depth == 0 || !group_has_content {
                    return false;
                }
                depth -= 1;
            }
            _ if depth == 0 => return false,
            _ => group_has_content = true,
        }
    }
    depth == 0 && group_has_content
}

fn yaml_scalar(raw: &str) -> &str {
    let raw = raw.trim();
    if raw.len() >= 2
        && ((raw.starts_with('\'') && raw.ends_with('\''))
            || (raw.starts_with('"') && raw.ends_with('"')))
    {
        &raw[1..raw.len() - 1]
    } else {
        raw
    }
}

fn inline_lockfile_integrity(line: &str) -> Option<&str> {
    let fields = line
        .trim()
        .strip_prefix("resolution: {")?
        .strip_suffix('}')?;
    fields
        .split(',')
        .find_map(|field| field.trim().strip_prefix("integrity: "))
        .map(yaml_scalar)
}

fn lockfile_integrity(profile: &Path, package_name: &str, version: &str) -> Result<String, String> {
    let path = profile.join("pnpm-lock.yaml");
    let bytes = crate::secure_fs::read_bounded(&path, PROFILE_LOCK_MAX_BYTES)?
        .ok_or_else(|| "pnpm lockfile is missing".to_string())?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| format!("pnpm lockfile is not valid UTF-8: {e}"))?;
    let expected_key = format!("{package_name}@{version}");
    let mut in_packages = false;
    let mut in_target = false;
    let mut in_resolution = false;
    for line in text.lines() {
        if line == "packages:" {
            in_packages = true;
            continue;
        }
        if !in_packages {
            continue;
        }
        if line == "snapshots:" {
            break;
        }
        if let Some(key) = lockfile_package_key(line) {
            in_target = lockfile_package_key_matches(key, &expected_key);
            in_resolution = false;
            continue;
        }
        if !in_target {
            continue;
        }
        if let Some(integrity) = inline_lockfile_integrity(line) {
            return Ok(integrity.to_string());
        }
        if line == "    resolution:" {
            in_resolution = true;
            continue;
        }
        if in_resolution {
            if let Some(integrity) = line.trim().strip_prefix("integrity: ") {
                return Ok(yaml_scalar(integrity).to_string());
            }
            if !line.starts_with("      ") {
                in_resolution = false;
            }
        }
    }
    Err(format!(
        "pnpm lockfile does not contain integrity for {package_name}@{version}"
    ))
}

/// A bundle patch is resolved by upstream relative to the installed package
/// directory. Keep the declaration portable and reject every form that could
/// escape that directory on either Unix or Windows before activation.
fn is_safe_bundle_patch_path(path: &str) -> bool {
    if path.is_empty()
        || path.len() > BUNDLE_PATCH_PATH_MAX_BYTES
        || path.starts_with('/')
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
    {
        return false;
    }
    let relative = path.strip_prefix("./").unwrap_or(path);
    !relative.is_empty()
        && relative
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn checked_installed_package_dir(profile: &Path, package_name: &str) -> Result<PathBuf, String> {
    let node_modules = profile.join("node_modules");
    checked_real_dir(&node_modules, "market profile node_modules")?;
    let package = if let Some((scope, name)) = package_name.split_once('/') {
        let scope_dir = node_modules.join(scope);
        checked_real_dir(&scope_dir, "market package scope directory")?;
        scope_dir.join(name)
    } else {
        node_modules.join(package_name)
    };
    checked_real_dir(&package, "installed market package directory")?;
    Ok(package)
}

/// Verify all installation facts pnpm materialized. No build script can run
/// in the market pnpm invocation, and normal pending verification requires
/// the profile to stay pre-disabled throughout this check.
pub(crate) fn verify_market_installation(
    dsh_home: &Path,
    candidate: &crate::market::MarketInstallCandidate,
    require_inactive: bool,
) -> Result<(), String> {
    let (profile, manifest) = read_profile_manifest(dsh_home)?;
    let spec = profile_dependencies(&manifest)?
        .get(&candidate.package_name)
        .and_then(Value::as_str)
        .ok_or_else(|| "pnpm did not add the expected market dependency".to_string())?;
    if spec != candidate.tarball {
        return Err(
            "pnpm saved a dependency source different from the reviewed tarball".to_string(),
        );
    }
    if require_inactive && profile_bundles(&manifest)?.contains(candidate.package_name.as_str()) {
        return Err("market package became active before explicit activation".to_string());
    }

    let installed_package = checked_installed_package_dir(&profile, &candidate.package_name)?;
    let installed_manifest = installed_package.join("package.json");
    let bytes = crate::secure_fs::read_bounded(&installed_manifest, INSTALLED_MANIFEST_MAX_BYTES)?
        .ok_or_else(|| "installed market package manifest is missing".to_string())?;
    let installed: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("installed market package has invalid JSON: {e}"))?;
    if installed.get("name").and_then(Value::as_str) != Some(candidate.package_name.as_str())
        || installed.get("version").and_then(Value::as_str) != Some(candidate.version.as_str())
    {
        return Err(
            "installed market package name/version differs from the reviewed source".to_string(),
        );
    }
    let patch = installed
        .get("dsh")
        .and_then(Value::as_object)
        .and_then(|dsh| dsh.get("bundle"))
        .and_then(Value::as_object)
        .and_then(|bundle| bundle.get("patch"))
        .and_then(Value::as_str)
        .ok_or_else(|| "installed market package is not a DSH bundle".to_string())?;
    if !is_safe_bundle_patch_path(patch) {
        return Err("installed market package has an unsafe dsh.bundle.patch path".to_string());
    }
    let patch_path = installed_package.join(patch);
    let package_root = std::fs::canonicalize(&installed_package)
        .map_err(|error| format!("cannot resolve installed market package directory: {error}"))?;
    let resolved_patch = std::fs::canonicalize(&patch_path).map_err(|error| {
        format!("cannot resolve installed market package bundle patch: {error}")
    })?;
    if !resolved_patch.starts_with(&package_root) || resolved_patch == package_root {
        return Err(
            "installed market package bundle patch escapes its package directory".to_string(),
        );
    }
    if crate::secure_fs::read_bounded(&patch_path, BUNDLE_PATCH_MAX_BYTES)?.is_none() {
        return Err("installed market package bundle patch is missing".to_string());
    }
    if lockfile_integrity(&profile, &candidate.package_name, &candidate.version)?
        != candidate.integrity
    {
        return Err("pnpm lockfile integrity differs from the reviewed source".to_string());
    }

    Ok(())
}

/// Verify all installation facts pnpm materialized before recording a pending
/// activation. No build script can run in the market pnpm invocation, and the
/// profile remains pre-disabled throughout this check.
pub fn verify_and_mark_market_pending(
    dsh_home: &Path,
    candidate: &crate::market::MarketInstallCandidate,
) -> Result<(), String> {
    verify_market_installation(dsh_home, candidate, true)?;

    let mut pending = load_pending(dsh_home)?;
    pending.plugins.insert(
        candidate.package_name.clone(),
        MarketPendingPlugin::from(candidate),
    );
    write_pending(dsh_home, &pending)
}

/// Explicit activation gate. The caller has freshly revalidated the catalog
/// candidate first; this function verifies the local pending record and
/// materialized package one more time before touching dsh.profile.bundles.
///
/// The bundle list is committed before the marker is removed. If the process
/// stops in between, the next read shows the package as active and a retry can
/// finish marker cleanup instead of leaving it falsely pending forever.
pub fn activate_market_plugin(
    dsh_home: &Path,
    candidate: &crate::market::MarketInstallCandidate,
) -> Result<(), String> {
    let mut pending = load_pending(dsh_home)?;
    let expected = pending
        .plugins
        .get(&candidate.package_name)
        .ok_or_else(|| "market package is not awaiting activation".to_string())?;
    if !expected.matches(candidate) {
        return Err(
            "market package pending state does not match the current catalog entry".to_string(),
        );
    }
    // A previous Activate can have completed the profile write just before a
    // process crash. In that recovery case accept the already-active bundle
    // after re-verifying its exact source, then clean up the marker below.
    verify_market_installation(dsh_home, candidate, false)?;

    // Persist rollback provenance before enabling the bundle. Inactive or
    // source-mismatched receipts are ignored and pruned by mutation paths; the
    // reverse ordering could leave an active market plugin that recovery
    // cannot revalidate.
    record_active_market_receipt(dsh_home, candidate)?;

    let (profile, mut manifest) = read_profile_manifest(dsh_home)?;
    let bundles = profile_bundles_mut(&mut manifest)?;
    for bundle in bundles.iter() {
        if bundle.as_str().is_none() {
            return Err("web profile bundles contains a non-string value".to_string());
        }
    }
    if !bundles
        .iter()
        .any(|bundle| bundle.as_str() == Some(candidate.package_name.as_str()))
    {
        bundles.push(Value::String(candidate.package_name.clone()));
        write_profile_manifest(&profile, &manifest)?;
    }

    // Keep the marker until after the activation write. A cleanup failure is
    // surfaced explicitly; the installed-list view prioritizes the active
    // bundle, so it will never invite a second activation.
    pending.plugins.remove(&candidate.package_name);
    write_pending(dsh_home, &pending).map_err(|error| {
        format!("plugin was activated, but pending-state cleanup failed: {error}")
    })?;
    Ok(())
}

/// Read package rows for the bootstrap UI. Failure is intentionally
/// non-fatal for the status page: the command returns an empty list while
/// mutation paths above remain fail-closed.
pub fn installed_plugins(dsh_home: &Path) -> Vec<InstalledPlugin> {
    let Ok((profile, manifest)) = read_profile_manifest(dsh_home) else {
        return Vec::new();
    };
    let Ok(dependencies) = profile_dependencies(&manifest) else {
        return Vec::new();
    };
    let bundles = profile_bundles(&manifest).unwrap_or_default();
    let pending = load_pending(dsh_home).unwrap_or_default();
    let mut names: Vec<&str> = dependencies
        .iter()
        .filter_map(|(name, spec)| {
            (is_valid_package_name(name) && spec.as_str().is_some_and(|value| !value.is_empty()))
                .then_some(name.as_str())
        })
        .collect();
    names.sort_unstable();
    names.truncate(MAX_INSTALLED_PLUGINS);

    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        let version = read_installed_plugin_version(&profile, name)
            .ok()
            .flatten()
            .unwrap_or_else(|| "—".to_string());
        let dependency_spec = dependencies.get(name).and_then(Value::as_str);
        let marker = pending.plugins.get(name).filter(|marker| {
            marker.package_name == name
                && marker.version == version
                && dependency_spec == Some(marker.tarball.as_str())
        });
        let (state, slug, entry_revision) = if bundles.contains(name) {
            ("active".to_string(), None, None)
        } else if let Some(marker) = marker {
            (
                "pending".to_string(),
                Some(marker.slug.clone()),
                Some(marker.entry_revision.clone()),
            )
        } else {
            ("installed".to_string(), None, None)
        };
        entries.push(InstalledPlugin {
            name: name.to_string(),
            version,
            state,
            slug,
            entry_revision,
        });
    }
    entries
}

fn read_installed_plugin_version(
    profile: &Path,
    package_name: &str,
) -> Result<Option<String>, String> {
    let path = checked_installed_package_dir(profile, package_name)?.join("package.json");
    let Some(bytes) = crate::secure_fs::read_bounded(&path, INSTALLED_MANIFEST_MAX_BYTES)? else {
        return Ok(None);
    };
    let package: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("installed plugin manifest is invalid JSON: {error}"))?;
    if package.get("name").and_then(Value::as_str) != Some(package_name) {
        return Err("installed plugin manifest name does not match its dependency".to_string());
    }
    let version = package
        .get("version")
        .and_then(Value::as_str)
        .filter(|version| {
            !version.is_empty() && version.len() <= 128 && !version.chars().any(char::is_control)
        })
        .ok_or_else(|| "installed plugin manifest has an invalid version".to_string())?;
    Ok(Some(version.to_string()))
}

/// The runner state managed by the shell: single-flight flag, cancellation
/// latch, app-exit latch, and the live child handle. `operation_gate` makes
/// begin/cancel/finish one atomic state transition so a cancel arriving
/// before the child handle is registered cannot be lost or applied to the
/// next operation.
pub struct PluginRunner {
    pub busy: AtomicBool,
    pub exiting: AtomicBool,
    cancel_requested: AtomicBool,
    terminating_child: AtomicBool,
    operation_gate: Mutex<()>,
    pub child: Mutex<Option<PlatformChild>>,
}

impl PluginRunner {
    pub fn new() -> Self {
        PluginRunner {
            busy: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            cancel_requested: AtomicBool::new(false),
            terminating_child: AtomicBool::new(false),
            operation_gate: Mutex::new(()),
            child: Mutex::new(None),
        }
    }

    pub fn try_begin(&self) -> bool {
        let _gate = self
            .operation_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.exiting.load(Ordering::SeqCst)
            || self.busy.load(Ordering::SeqCst)
            || self.terminating_child.load(Ordering::SeqCst)
        {
            return false;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        self.busy.store(true, Ordering::SeqCst);
        true
    }

    pub fn finish(&self) {
        self.finish_with(|| ());
    }

    /// Publish a completion side effect before another operation can claim
    /// the single-flight gate. In particular, queue `plugin-done` while the
    /// transition is still exclusive so a delayed event cannot belong to a
    /// newly started operation.
    pub fn finish_with<T>(&self, action: impl FnOnce() -> T) -> T {
        let _gate = self
            .operation_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.cancel_requested.store(false, Ordering::SeqCst);
        self.busy.store(false, Ordering::SeqCst);
        action()
    }

    pub fn request_cancel(&self) -> (bool, Option<PlatformChild>) {
        let _gate = self
            .operation_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.busy.load(Ordering::SeqCst)
            || self.exiting.load(Ordering::SeqCst)
            || self.cancel_requested.load(Ordering::SeqCst)
        {
            return (false, None);
        }
        self.cancel_requested.store(true, Ordering::SeqCst);
        let child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.terminating_child
            .store(child.is_some(), Ordering::SeqCst);
        (true, child)
    }

    pub fn child_termination_finished(&self) {
        self.terminating_child.store(false, Ordering::SeqCst);
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst) || self.terminating_child.load(Ordering::SeqCst)
    }

    pub fn cancellation_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }

    /// Run a short profile boundary only when no plugin/recovery mutation is
    /// active. Holding the transition gate for the action prevents a new
    /// operation from starting between the idle check and the protected read
    /// or write, without misreporting the boundary itself as a plugin job.
    pub fn with_idle_profile<T>(
        &self,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = self
            .operation_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.exiting.load(Ordering::SeqCst)
            || self.busy.load(Ordering::SeqCst)
            || self.terminating_child.load(Ordering::SeqCst)
        {
            return Err("插件操作仍在进行，完成或取消后才能重启 Harness".to_string());
        }
        action()
    }

    /// App-exit cleanup (C1 process-tree guarantee): a running plugin
    /// mutation tree is a separate process group / Job Object from the sidecar's
    /// Harness tree, so it must be killed explicitly — on unix it would
    /// otherwise be orphaned once the shell exits. Taking the handle also
    /// keeps the done-path from racing the kill. Polite signal first, then
    /// hard kill: the app is going away, there is no grace period to wait.
    ///
    /// The `exiting` latch closes the spawn/store race: run_plugin_op
    /// spawns the tree BEFORE storing the handle here, so an exit inside
    /// that window would find `child == None` — the latch makes the spawn
    /// path kill the fresh tree itself.
    pub fn shutdown(&self) {
        let _gate = self
            .operation_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.exiting.store(true, Ordering::SeqCst);
        self.cancel_requested.store(true, Ordering::SeqCst);
        if let Some(child) = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = child.graceful();
            child.force();
        }
    }
}

impl Default for PluginRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn active_market_receipt_round_trips_for_recovery_provenance() {
        let home = std::env::temp_dir().join(format!(
            "dshd-active-receipt-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        let receipt = MarketPendingPlugin::from(&candidate);
        let mut active = MarketActiveFile::default();
        active
            .plugins
            .insert(receipt.package_name.clone(), receipt.clone());
        write_active(&home, &active).unwrap();
        assert_eq!(
            active_market_receipt(&home, "fixture-plugin")
                .unwrap()
                .map(|value| value.entry_revision),
            Some("revision-1".to_string())
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn active_market_receipts_are_pruned_after_deactivation_or_source_replacement() {
        let home = std::env::temp_dir().join(format!(
            "dshd-active-receipt-prune-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        let receipt = MarketPendingPlugin::from(&candidate);
        let mut active = MarketActiveFile::default();
        active
            .plugins
            .insert(receipt.package_name.clone(), receipt.clone());
        write_active(&home, &active).unwrap();

        let (profile, mut manifest) = read_profile_manifest(&home).unwrap();
        manifest["dependencies"][&candidate.package_name] = Value::String("2.0.0".to_string());
        write_profile_manifest(&profile, &manifest).unwrap();
        assert!(active_market_receipt(&home, &candidate.package_name)
            .unwrap()
            .is_none());
        reconcile_active_market_receipts(&home).unwrap();
        assert!(load_active(&home).unwrap().plugins.is_empty());

        setup_market_profile(&home, &candidate);
        active.plugins.insert(receipt.package_name.clone(), receipt);
        write_active(&home, &active).unwrap();
        pre_disable_market_plugin(&home, &candidate).unwrap();
        assert!(load_active(&home).unwrap().plugins.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn recovery_reactivation_restores_provenance_before_the_bundle() {
        let home = std::env::temp_dir().join(format!(
            "dshd-recovery-receipt-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        record_active_market_receipt(&home, &candidate).unwrap();

        pre_disable_plugin(&home, &candidate.package_name).unwrap();
        reconcile_market_receipts(&home).unwrap();
        assert!(load_active(&home).unwrap().plugins.is_empty());

        // The command's market-gated recovery path records provenance first,
        // then restores the exact backed-up bundle profile.
        record_active_market_receipt(&home, &candidate).unwrap();
        let (profile, mut manifest) = read_profile_manifest(&home).unwrap();
        profile_bundles_mut(&mut manifest)
            .unwrap()
            .push(Value::String(candidate.package_name.clone()));
        write_profile_manifest(&profile, &manifest).unwrap();
        reconcile_market_receipts(&home).unwrap();

        assert_eq!(
            active_market_receipt(&home, &candidate.package_name)
                .unwrap()
                .as_ref(),
            Some(&MarketPendingPlugin::from(&candidate))
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn package_names_accept_reject() {
        for ok in [
            "is-odd",
            "lodash",
            "@cordisjs/plugin-example",
            "@scope/pkg",
            "@s/pkg.sub_2",
            "a",
            "a1.-_b",
            "x".repeat(214).as_str(),
        ] {
            assert!(is_valid_package_name(ok), "should accept {ok:?}");
        }
        for bad in [
            "",
            "Is-Odd",
            "pkg space",
            "pkg!",
            "pkg&",
            "@scope",
            "@scope/",
            "@/pkg",
            "@scope//x",
            "a/b",
            ".pkg",
            "_pkg",
            "pkg/",
            "pkg:rm",
            "pkg`whoami`",
            "pkg$(echo x)",
            "-g",
            "-D",
            "--save-dev",
            "--global",
            "-h",
            "--help",
            "--",
            "-",
            "@scope/pkg@1",
            "@Scope/pkg",
            "@scope/.hidden",
            "@scope/-hidden",
            "y".repeat(215).as_str(),
            format!("@scope/{}", "z".repeat(215)).as_str(),
        ] {
            assert!(!is_valid_package_name(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn runner_latches_cancel_before_a_child_is_registered() {
        let runner = PluginRunner::new();
        assert!(runner.try_begin());
        assert!(runner.busy.load(Ordering::SeqCst));
        assert!(runner.request_cancel().0);
        assert!(runner.cancellation_requested());
        assert!(
            !runner.request_cancel().0,
            "repeated cancellation must be idempotent"
        );
        assert!(!runner.try_begin(), "single-flight must remain held");

        runner.finish();
        assert!(!runner.busy.load(Ordering::SeqCst));
        assert!(!runner.cancellation_requested());
        assert!(
            !runner.request_cancel().0,
            "idle cancel must not poison the next run"
        );
        assert!(runner.try_begin());
        assert!(!runner.cancellation_requested());
        runner.finish();
    }

    #[test]
    fn terminating_child_keeps_the_single_flight_gate_closed() {
        let runner = PluginRunner::new();
        assert!(runner.try_begin());
        runner.terminating_child.store(true, Ordering::SeqCst);
        runner.finish();
        assert!(runner.is_busy());
        assert!(!runner.try_begin());
        runner.child_termination_finished();
        assert!(!runner.is_busy());
        assert!(runner.try_begin());
        runner.finish();
    }

    #[test]
    fn idle_profile_boundary_does_not_overlap_a_plugin_mutation() {
        let runner = PluginRunner::new();
        assert_eq!(runner.with_idle_profile(|| Ok(7)).unwrap(), 7);
        assert!(!runner.is_busy(), "a restart boundary is not a plugin job");

        assert!(runner.try_begin());
        let mut called = false;
        let error = runner
            .with_idle_profile(|| {
                called = true;
                Ok(())
            })
            .unwrap_err();
        assert!(error.contains("插件操作仍在进行"), "{error}");
        assert!(!called, "busy profile boundary must not run its action");
        runner.finish();
    }

    #[test]
    fn completion_is_published_before_the_next_operation_can_begin() {
        let runner = std::sync::Arc::new(PluginRunner::new());
        assert!(runner.try_begin());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let finish_runner = runner.clone();
        let finisher = std::thread::spawn(move || {
            finish_runner.finish_with(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        entered_rx.recv().unwrap();

        let (begin_tx, begin_rx) = std::sync::mpsc::channel();
        let begin_runner = runner.clone();
        let beginner = std::thread::spawn(move || {
            begin_tx.send(begin_runner.try_begin()).unwrap();
        });
        assert!(
            begin_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "the next operation must wait until completion publication returns"
        );
        release_tx.send(()).unwrap();
        finisher.join().unwrap();
        assert!(begin_rx.recv().unwrap());
        beginner.join().unwrap();
        runner.finish();
    }

    #[test]
    fn shell_safe_spec_rejects_metacharacters() {
        assert!(is_shell_safe_spec("file:C:/Users/me/.dsh/sideload-1.tgz"));
        assert!(!is_shell_safe_spec(
            "file:C:/Users/me&you/.dsh/sideload-1.tgz"
        ));
        assert!(!is_shell_safe_spec(
            "file:C:/Users/me you/.dsh/sideload-1.tgz"
        ));
        assert!(!is_shell_safe_spec(
            "file:C:/Users/me|you/.dsh/sideload-1.tgz"
        ));
    }

    #[test]
    fn stage_sideload_uses_safe_generated_name() {
        let home = std::env::temp_dir().join(format!("dsh-stage-sideload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let src = home.join("bad&name.tgz");
        std::fs::write(&src, b"fake").unwrap();
        let staged = stage_sideload(&home, &src).expect("stage sideload");
        assert_eq!(
            staged.parent().unwrap(),
            &home.join(".desktop-tools").join("sideload")
        );
        let name = staged.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("sideload-") && name.ends_with(".tgz"));
        assert!(name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'));
        assert!(staged.is_file());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn shim_texts_quote_paths_and_forward_args() {
        assert_eq!(
            pnpm_shim_script("/opt/node", "/opt/harness/node_modules/pnpm/bin/pnpm.cjs"),
            "#!/bin/sh\nexec '/opt/node' '/opt/harness/node_modules/pnpm/bin/pnpm.cjs' \"$@\"\n"
        );
        assert_eq!(
            pnpm_shim_cmd("C:\\node.exe", "C:\\harness\\pnpm.cjs"),
            "@echo off\nsetlocal DisableDelayedExpansion\n\"C:\\node.exe\" \"C:\\harness\\pnpm.cjs\" %*\n"
        );
    }

    #[test]
    fn shim_texts_escape_shell_sensitive_path_characters() {
        assert_eq!(
            pnpm_shim_script("/tmp/a'b/$node", "/tmp/$(touch nope)/pnpm.cjs"),
            "#!/bin/sh\nexec '/tmp/a'\"'\"'b/$node' '/tmp/$(touch nope)/pnpm.cjs' \"$@\"\n"
        );
        assert_eq!(
            pnpm_shim_cmd("C:\\100%\\node.exe", "C:\\bang!\\pnpm.cjs"),
            "@echo off\nsetlocal DisableDelayedExpansion\n\"C:\\100%%\\node.exe\" \"C:\\bang!\\pnpm.cjs\" %*\n"
        );
    }

    /// End-to-end shim proof (plan I4): only runs when the runtime is staged
    /// (smoke/dev machines) — the unit job has an empty resources/runtime
    /// dir, so it skips there instead of failing.
    #[test]
    fn shim_runs_bundled_pnpm_when_runtime_staged() {
        let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/runtime");
        let node = runtime.join(format!("node{}", if cfg!(windows) { ".exe" } else { "" }));
        let pnpm_cjs = runtime
            .join("harness")
            .join("node_modules")
            .join("pnpm")
            .join("bin")
            .join("pnpm.cjs");
        if !node.is_file() || !pnpm_cjs.is_file() {
            eprintln!("skipping: staged runtime not present (node or bundled pnpm missing)");
            return;
        }
        let home = std::env::temp_dir().join(format!("dsd-shim-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let dir = ensure_pnpm_shim(&home, &node, &pnpm_cjs).expect("shim write");
        let shim = if cfg!(windows) {
            dir.join("pnpm.cmd")
        } else {
            dir.join("pnpm")
        };
        let output = std::process::Command::new(&shim)
            .arg("--version")
            .output()
            .expect("shim exec");
        assert!(
            output.status.success(),
            "shim exited {:?}\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(!String::from_utf8_lossy(&output.stdout).trim().is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn shim_writer_refuses_a_symlinked_leaf() {
        use std::os::unix::fs::symlink;
        let home = std::env::temp_dir().join(format!(
            "dsd-shim-link-test-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        std::fs::create_dir_all(home.join(".desktop-tools")).unwrap();
        let outside = home.join("outside");
        std::fs::write(&outside, b"keep").unwrap();
        symlink(&outside, home.join(".desktop-tools/pnpm")).unwrap();
        let error = ensure_pnpm_shim(
            &home,
            std::path::Path::new("/opt/node"),
            std::path::Path::new("/opt/pnpm.cjs"),
        )
        .unwrap_err();
        assert!(error.contains("regular file"), "{error}");
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        std::fs::remove_dir_all(home).unwrap();
    }

    fn market_candidate() -> crate::market::MarketInstallCandidate {
        crate::market::MarketInstallCandidate {
            slug: "fixture-plugin".to_string(),
            entry_revision: "revision-1".to_string(),
            package_name: "fixture-plugin".to_string(),
            version: "1.0.0".to_string(),
            integrity: "sha512-CQpnWPrDwmP1+SMHXZhtLtJv90yiyVfluGsX5iNCVkrhQtU3TQHsUWPG9wkdk9Lgd5yNpAg9jQEo90CBaXgWMA==".to_string(),
            registry: "https://registry.npmjs.org".to_string(),
            tarball: "https://registry.npmjs.org/fixture-plugin/-/fixture-plugin-1.0.0.tgz".to_string(),
        }
    }

    fn setup_market_profile(home: &Path, candidate: &crate::market::MarketInstallCandidate) {
        let profile = home.join("profiles").join("web");
        std::fs::create_dir_all(profile.join("node_modules").join(&candidate.package_name))
            .unwrap();
        let mut dependencies = serde_json::Map::new();
        dependencies.insert(
            candidate.package_name.clone(),
            Value::String(candidate.tarball.clone()),
        );
        let manifest = serde_json::json!({
            "name": "dsh-profile-web",
            "private": true,
            "dependencies": dependencies,
            "dsh": {"profile": {"bundles": [candidate.package_name.clone()]}}
        });
        std::fs::write(
            profile.join("package.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let package = serde_json::json!({
            "name": candidate.package_name,
            "version": candidate.version,
            "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}
        });
        std::fs::write(
            profile
                .join("node_modules")
                .join(&candidate.package_name)
                .join("package.json"),
            serde_json::to_string_pretty(&package).unwrap(),
        )
        .unwrap();
        std::fs::write(
            profile
                .join("node_modules")
                .join(&candidate.package_name)
                .join("cordis.patch.yml"),
            "[]\n",
        )
        .unwrap();
        std::fs::write(
            profile.join("pnpm-lock.yaml"),
            format!(
                "lockfileVersion: '9.0'\n\npackages:\n\n  {}@{}:\n    resolution: {{integrity: {}}}\n\nsnapshots:\n",
                candidate.package_name, candidate.version, candidate.integrity
            ),
        )
        .unwrap();
    }

    #[test]
    fn existing_pnpm_store_is_reused_without_changing_layout() {
        let home = std::env::temp_dir().join(format!(
            "dsh-pnpm-store-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let profile = home.join("profiles/web");
        let node_modules = profile.join("node_modules");
        std::fs::create_dir_all(&node_modules).unwrap();
        std::fs::write(
            profile.join("package.json"),
            r#"{"dependencies":{},"dsh":{"profile":{"bundles":[]}}}"#,
        )
        .unwrap();
        let store_base = home.join("shared-pnpm-store");
        let versioned = store_base.join("v11");
        std::fs::write(
            node_modules.join(".modules.yaml"),
            serde_json::to_vec(&serde_json::json!({"storeDir": versioned})).unwrap(),
        )
        .unwrap();
        assert_eq!(pnpm_store_base(&profile, 11).unwrap(), Some(store_base));
        assert_eq!(
            generic_profile_store_base(&home, 11).unwrap(),
            Some(home.join("shared-pnpm-store"))
        );

        let wrong = home.join("legacy-store/v10");
        std::fs::write(
            node_modules.join(".modules.yaml"),
            serde_json::to_vec(&serde_json::json!({"storeDir": wrong})).unwrap(),
        )
        .unwrap();
        let error = pnpm_store_base(&profile, 11).unwrap_err();
        assert!(error.contains("incompatible pnpm store layout"), "{error}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pending_receipt_repairs_accidental_activation_and_blocks_generic_add() {
        let home = std::env::temp_dir().join(format!(
            "dsh-pending-activation-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        pre_disable_market_plugin(&home, &candidate).unwrap();
        verify_and_mark_market_pending(&home, &candidate).unwrap();

        // Simulate the old upstream global reconciliation adding the pending
        // dependency while an unrelated plugin was changed.
        let (profile, mut manifest) = read_profile_manifest(&home).unwrap();
        profile_bundles_mut(&mut manifest)
            .unwrap()
            .push(Value::String(candidate.package_name.clone()));
        write_profile_manifest(&profile, &manifest).unwrap();

        let error = ensure_no_pending_market_plugins(&home).unwrap_err();
        assert!(error.contains("awaiting explicit activation"), "{error}");
        let (_, repaired) = read_profile_manifest(&home).unwrap();
        assert!(!profile_bundles(&repaired)
            .unwrap()
            .contains(candidate.package_name.as_str()));
        assert!(load_pending(&home)
            .unwrap()
            .plugins
            .contains_key(&candidate.package_name));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn uninstall_pre_disable_requires_a_dependency_and_changes_only_its_target() {
        let home = std::env::temp_dir().join(format!(
            "dsh-exact-pre-disable-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        let (profile, mut manifest) = read_profile_manifest(&home).unwrap();
        profile_bundles_mut(&mut manifest)
            .unwrap()
            .push(Value::String("other-plugin".to_string()));
        write_profile_manifest(&profile, &manifest).unwrap();

        let error = pre_disable_installed_plugin(&home, "other-plugin").unwrap_err();
        assert!(error.contains("not an installed"), "{error}");
        pre_disable_installed_plugin(&home, &candidate.package_name).unwrap();
        let (_, manifest) = read_profile_manifest(&home).unwrap();
        let bundles = profile_bundles(&manifest).unwrap();
        assert!(!bundles.contains(candidate.package_name.as_str()));
        assert!(bundles.contains("other-plugin"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn generic_add_refuses_a_symlinked_node_modules_before_spawn() {
        use std::os::unix::fs::symlink;

        let home = std::env::temp_dir().join(format!(
            "dsh-generic-layout-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let profile = home.join("profiles/web");
        let outside = home.join("outside");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(outside.join("is-odd")).unwrap();
        std::fs::write(
            profile.join("package.json"),
            r#"{"dependencies":{"is-odd":"3.0.1"},"dsh":{"profile":{"bundles":[]}}}"#,
        )
        .unwrap();
        std::fs::write(
            outside.join("is-odd/package.json"),
            r#"{"name":"is-odd","version":"9.9.9"}"#,
        )
        .unwrap();
        symlink(&outside, profile.join("node_modules")).unwrap();

        let error = ensure_no_pending_market_plugins(&home).unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
        let rows = installed_plugins(&home);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].version, "—");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn installed_list_uses_production_parser_and_rejects_path_like_dependency_keys() {
        let home = std::env::temp_dir().join(format!(
            "dsh-installed-list-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let profile = home.join("profiles").join("web");
        std::fs::create_dir_all(profile.join("node_modules/is-odd")).unwrap();
        std::fs::create_dir_all(profile.join("outside")).unwrap();
        std::fs::write(
            profile.join("package.json"),
            r#"{
              "dependencies": {
                "../outside": "9.9.9",
                "is-odd": "^3.0.1",
                "not-a-string": false,
                "zz-top": "1.0.0"
              },
              "dsh": {"profile": {"bundles": []}}
            }"#,
        )
        .unwrap();
        std::fs::write(
            profile.join("node_modules/is-odd/package.json"),
            r#"{"name":"is-odd","version":"3.0.1"}"#,
        )
        .unwrap();
        // This is exactly where the old `node_modules.join("../outside")`
        // traversal landed. Its version must never become a bootstrap row.
        std::fs::write(
            profile.join("outside/package.json"),
            r#"{"name":"../outside","version":"9.9.9"}"#,
        )
        .unwrap();

        let rows = installed_plugins(&home);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "is-odd");
        assert_eq!(rows[0].version, "3.0.1");
        assert_eq!(rows[1].name, "zz-top");
        assert_eq!(rows[1].version, "—");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn installed_list_does_not_trust_a_mismatched_or_oversized_package_manifest() {
        let home = std::env::temp_dir().join(format!(
            "dsh-installed-manifest-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let profile = home.join("profiles").join("web");
        std::fs::create_dir_all(profile.join("node_modules/is-odd")).unwrap();
        std::fs::write(
            profile.join("package.json"),
            r#"{"dependencies":{"is-odd":"3.0.1"},"dsh":{"profile":{"bundles":[]}}}"#,
        )
        .unwrap();
        std::fs::write(
            profile.join("node_modules/is-odd/package.json"),
            r#"{"name":"different-package","version":"3.0.1"}"#,
        )
        .unwrap();
        assert_eq!(installed_plugins(&home)[0].version, "—");

        std::fs::write(
            profile.join("node_modules/is-odd/package.json"),
            vec![b' '; INSTALLED_MANIFEST_MAX_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(installed_plugins(&home)[0].version, "—");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn profile_manifest_reads_are_bounded() {
        let home = std::env::temp_dir().join(format!(
            "dsh-profile-bound-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let profile = home.join("profiles").join("web");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("package.json"),
            vec![b' '; PROFILE_MANIFEST_MAX_BYTES as usize + 1],
        )
        .unwrap();
        let error = read_profile_manifest(&home).unwrap_err();
        assert!(error.contains("byte limit"), "{error}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn market_install_rejects_local_patched_dependency_transforms() {
        let home = std::env::temp_dir().join(format!(
            "dsh-market-patched-dependency-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        let (profile, mut manifest) = read_profile_manifest(&home).unwrap();
        manifest["pnpm"] = serde_json::json!({
            "patchedDependencies": {
                "fixture-plugin@1.0.0": "patches/fixture.patch"
            }
        });
        write_profile_manifest(&profile, &manifest).unwrap();

        let error = ensure_market_install_config(&home).unwrap_err();
        assert!(error.contains("patchedDependencies"), "{error}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn market_install_rejects_profile_local_npmrc_redirects() {
        let home = std::env::temp_dir().join(format!(
            "dsh-market-npmrc-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        std::fs::write(home.join("profiles/web/.npmrc"), "global=true\n").unwrap();

        let error = ensure_market_install_config(&home).unwrap_err();
        assert!(error.contains(".npmrc"), "{error}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pending_state_requires_the_exact_reviewed_dependency_source() {
        let home = std::env::temp_dir().join(format!(
            "dsh-pending-source-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        pre_disable_market_plugin(&home, &candidate).unwrap();
        verify_and_mark_market_pending(&home, &candidate).unwrap();
        assert_eq!(installed_plugins(&home)[0].state, "pending");

        let (profile, mut manifest) = read_profile_manifest(&home).unwrap();
        manifest["dependencies"][&candidate.package_name] =
            Value::String(candidate.version.clone());
        profile_bundles_mut(&mut manifest)
            .unwrap()
            .push(Value::String(candidate.package_name.clone()));
        write_profile_manifest(&profile, &manifest).unwrap();
        assert_eq!(installed_plugins(&home)[0].state, "active");
        reconcile_market_receipts(&home).unwrap();
        assert!(load_pending(&home).unwrap().plugins.is_empty());
        assert!(!profile_bundles(&read_profile_manifest(&home).unwrap().1)
            .unwrap()
            .contains(candidate.package_name.as_str()));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pending_reconciliation_prunes_an_unreadable_installed_manifest() {
        let home = std::env::temp_dir().join(format!(
            "dsh-pending-invalid-manifest-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        pre_disable_market_plugin(&home, &candidate).unwrap();
        verify_and_mark_market_pending(&home, &candidate).unwrap();
        std::fs::write(
            home.join("profiles/web/node_modules")
                .join(&candidate.package_name)
                .join("package.json"),
            "not json",
        )
        .unwrap();

        reconcile_market_receipts(&home).unwrap();
        assert!(load_pending(&home).unwrap().plugins.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn market_verifier_rejects_bundle_patch_path_escape() {
        let home = std::env::temp_dir().join(format!(
            "dsh-market-patch-escape-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        pre_disable_market_plugin(&home, &candidate).unwrap();
        let manifest_path = home
            .join("profiles/web/node_modules")
            .join(&candidate.package_name)
            .join("package.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&serde_json::json!({
                "name": candidate.package_name,
                "version": candidate.version,
                "dsh": {"bundle": {"patch": "../../../outside.yml"}}
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(home.join("outside.yml"), "[]\n").unwrap();

        let error = verify_market_installation(&home, &candidate, true).unwrap_err();
        assert!(error.contains("unsafe dsh.bundle.patch"), "{error}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn bundle_patch_path_contract_is_cross_platform() {
        for valid in ["cordis.patch.yml", "./cordis.patch.yml", "patches/base.yml"] {
            assert!(is_safe_bundle_patch_path(valid), "should accept {valid:?}");
        }
        for invalid in [
            "",
            "/etc/passwd",
            "../outside.yml",
            "a/../../outside.yml",
            "./../outside.yml",
            "C:/Windows/system.ini",
            "..\\outside.yml",
            "patches//base.yml",
            "patches/./base.yml",
        ] {
            assert!(
                !is_safe_bundle_patch_path(invalid),
                "should reject {invalid:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn market_verifier_rejects_bundle_patch_symlink_escape() {
        use std::os::unix::fs::symlink;

        let home = std::env::temp_dir().join(format!(
            "dsh-market-patch-link-{}",
            crate::secure_fs::random_suffix().unwrap()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        pre_disable_market_plugin(&home, &candidate).unwrap();
        let package = home
            .join("profiles/web/node_modules")
            .join(&candidate.package_name);
        let outside = home.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("patch.yml"), "[]\n").unwrap();
        symlink(&outside, package.join("patches")).unwrap();
        std::fs::write(
            package.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": candidate.package_name,
                "version": candidate.version,
                "dsh": {"bundle": {"patch": "patches/patch.yml"}}
            }))
            .unwrap(),
        )
        .unwrap();

        let error = verify_market_installation(&home, &candidate, true).unwrap_err();
        assert!(error.contains("escapes its package directory"), "{error}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn market_lockfile_reads_quoted_scoped_pnpm_package_keys() {
        let home =
            std::env::temp_dir().join(format!("dsh-market-scoped-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let profile = home.join("profiles").join("web");
        std::fs::create_dir_all(&profile).unwrap();
        let candidate = crate::market::MarketInstallCandidate {
            package_name: "@tauri-apps/api".to_string(),
            version: "2.11.1".to_string(),
            ..market_candidate()
        };
        std::fs::write(
            profile.join("pnpm-lock.yaml"),
            format!(
                "lockfileVersion: '9.0'\n\npackages:\n\n  '{}@{}':\n    resolution: {{integrity: {}}}\n\nsnapshots:\n",
                candidate.package_name, candidate.version, candidate.integrity
            ),
        )
        .unwrap();
        assert_eq!(
            lockfile_integrity(&profile, &candidate.package_name, &candidate.version)
                .expect("quoted scoped package key should resolve"),
            candidate.integrity
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn market_lockfile_reads_exact_version_with_pnpm_peer_context() {
        let home = std::env::temp_dir().join(format!(
            "dsh-market-peer-context-lock-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let profile = home.join("profiles").join("web");
        std::fs::create_dir_all(&profile).unwrap();
        let candidate = crate::market::MarketInstallCandidate {
            package_name: "@scope/demo".to_string(),
            version: "1.0.0".to_string(),
            ..market_candidate()
        };
        std::fs::write(
            profile.join("pnpm-lock.yaml"),
            format!(
                "lockfileVersion: '9.0'\n\npackages:\n\n  '{}@{}(react@18.3.1)(typescript@5.7.3)':\n    resolution:\n      integrity: {}\n\nsnapshots:\n",
                candidate.package_name, candidate.version, candidate.integrity
            ),
        )
        .unwrap();
        assert_eq!(
            lockfile_integrity(&profile, &candidate.package_name, &candidate.version)
                .expect("exact package version with peer context should resolve"),
            candidate.integrity
        );
        assert!(!lockfile_package_key_matches(
            "@scope/demo@1.0.00(react@18.3.1)",
            "@scope/demo@1.0.0"
        ));
        assert!(!lockfile_package_key_matches(
            "@scope/demo@1.0.0(peer context)",
            "@scope/demo@1.0.0"
        ));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn market_package_stays_pending_until_explicit_activation() {
        let home = std::env::temp_dir().join(format!("dsh-market-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);

        pre_disable_market_plugin(&home, &candidate).expect("pre-disable");
        let (_, manifest) = read_profile_manifest(&home).expect("profile after pre-disable");
        assert!(!profile_bundles(&manifest)
            .expect("bundle list")
            .contains(candidate.package_name.as_str()));

        verify_and_mark_market_pending(&home, &candidate).expect("verified pending");
        let rows = installed_plugins(&home);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, "pending");
        let (_, pending_manifest) = read_profile_manifest(&home).expect("pending profile");
        assert!(!profile_bundles(&pending_manifest)
            .expect("bundle list")
            .contains(candidate.package_name.as_str()));

        activate_market_plugin(&home, &candidate).expect("explicit activation");
        let rows = installed_plugins(&home);
        assert_eq!(rows[0].state, "active");
        let (_, active_manifest) = read_profile_manifest(&home).expect("active profile");
        assert!(profile_bundles(&active_manifest)
            .expect("bundle list")
            .contains(candidate.package_name.as_str()));
        assert!(load_pending(&home)
            .expect("pending state")
            .plugins
            .is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn activation_recovers_if_marker_cleanup_was_interrupted() {
        let home = std::env::temp_dir().join(format!(
            "dsh-market-activate-recovery-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let candidate = market_candidate();
        setup_market_profile(&home, &candidate);
        pre_disable_market_plugin(&home, &candidate).expect("pre-disable");
        verify_and_mark_market_pending(&home, &candidate).expect("verified pending");

        // Activation persists exact provenance before enabling the bundle.
        // Simulate a crash after both writes but before the pending marker can
        // be removed.
        let mut active = MarketActiveFile::default();
        active.plugins.insert(
            candidate.package_name.clone(),
            MarketPendingPlugin::from(&candidate),
        );
        write_active(&home, &active).expect("simulate active receipt write");
        let (profile, mut manifest) = read_profile_manifest(&home).expect("profile");
        profile_bundles_mut(&mut manifest)
            .expect("bundle list")
            .push(Value::String(candidate.package_name.clone()));
        write_profile_manifest(&profile, &manifest).expect("simulate activation write");

        assert_eq!(installed_plugins(&home)[0].state, "active");
        reconcile_market_receipts(&home).expect("startup reconciliation");
        assert!(load_pending(&home)
            .expect("pending state")
            .plugins
            .is_empty());
        assert!(
            profile_bundles(&read_profile_manifest(&home).expect("reconciled profile").1)
                .expect("bundle list")
                .contains(candidate.package_name.as_str())
        );
        assert_eq!(
            load_active(&home)
                .expect("active state")
                .plugins
                .get(&candidate.package_name),
            Some(&MarketPendingPlugin::from(&candidate))
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
