//! In-app plugin installation via the official `dsh plugin` CLI.
//!
//! Process-tree guarantees come from reusing dsh-sidecar's `PlatformChild`
//! (unix process group / Windows Job Object): cancel and app-exit always
//! clean the whole node → dsh → pnpm → node-gyp tree. Upstream's init /
//! reconcile logic is untouched — we only make sure it finds a pnpm.

use dsh_sidecar::platform::PlatformChild;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
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
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot inspect sideload file: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err("sideload file must not be a symlink".to_string());
    }
    if !meta.is_file() {
        return Err("sideload path is not a regular file".to_string());
    }
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
    let dir = tools.join("sideload");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err("refusing to use symlinked sideload dir".to_string())
        }
        Ok(meta) if !meta.is_dir() => return Err("sideload is not a directory".to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create sideload dir: {e}"))?;
        }
        Err(e) => return Err(format!("cannot inspect sideload dir: {e}")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot chmod sideload dir: {e}"))?;
    }
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dest = dir.join(format!("sideload-{millis}-{}.tgz", std::process::id()));
    std::fs::copy(src, &dest).map_err(|e| format!("cannot stage sideload file: {e}"))?;
    Ok(dest)
}

/// Pure shim-text generation (unit-tested).
pub fn pnpm_shim_script(node: &str, pnpm_cjs: &str) -> String {
    format!("#!/bin/sh\nexec \"{node}\" \"{pnpm_cjs}\" \"$@\"\n")
}

pub fn pnpm_shim_cmd(node: &str, pnpm_cjs: &str) -> String {
    format!("@echo off\n\"{node}\" \"{pnpm_cjs}\" %*\n")
}

/// Ensure `<dsh_home>/.desktop-tools/` holds the pnpm shims; returns the
/// directory to prepend to PATH for the plugin child. Both shims are written
/// on every platform: cmd.exe resolves `pnpm` via PATHEXT to `pnpm.cmd`
/// (extensionless files are never executed there, so the unix script is
/// inert), while the `.cmd` variant also covers git-bash on Windows.
pub fn ensure_pnpm_shim(dsh_home: &Path, node: &Path, pnpm_cjs: &Path) -> Result<PathBuf, String> {
    let dir = dsh_home.join(".desktop-tools");
    // Same write-path stance as DSH_HOME / the preset root: never write
    // shims (or chmod) through a symlink someone planted at the tools dir.
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(
                "tools dir is a symbolic link — refusing to write pnpm shims; remove the link"
                    .to_string(),
            );
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect tools dir: {e}")),
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create tools dir: {e}"))?;
    let node_s = node.display().to_string();
    let cjs_s = pnpm_cjs.display().to_string();
    let unix = dir.join("pnpm");
    std::fs::write(&unix, pnpm_shim_script(&node_s, &cjs_s)).map_err(|e| e.to_string())?;
    let win = dir.join("pnpm.cmd");
    std::fs::write(&win, pnpm_shim_cmd(&node_s, &cjs_s)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The tools dir holds executables we generate: keep it 0700 like
        // DSH_HOME itself, and mark the unix shim executable.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&unix, std::fs::Permissions::from_mode(0o755))
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
const MARKET_PENDING_MAX_BYTES: u64 = 256 * 1024;
const PROFILE_LOCK_MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fn matches(&self, candidate: &crate::market::MarketInstallCandidate) -> bool {
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
    if metadata.file_type().is_symlink() {
        return Err(format!("{label} must not be a symbolic link"));
    }
    if !metadata.is_dir() {
        return Err(format!("{label} is not a directory"));
    }
    Ok(())
}

fn market_tools_dir(dsh_home: &Path) -> Result<PathBuf, String> {
    checked_real_dir(dsh_home, "DSH_HOME")?;
    let tools = dsh_home.join(".desktop-tools");
    match std::fs::symlink_metadata(&tools) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("refusing to use symlinked .desktop-tools".to_string())
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(".desktop-tools is not a directory".to_string())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&tools)
                .map_err(|e| format!("cannot create .desktop-tools: {e}"))?;
        }
        Err(error) => return Err(format!("cannot inspect .desktop-tools: {error}")),
    }
    checked_real_dir(&tools, ".desktop-tools")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tools, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot chmod .desktop-tools: {e}"))?;
    }
    Ok(tools)
}

fn pending_path(dsh_home: &Path) -> Result<PathBuf, String> {
    Ok(market_tools_dir(dsh_home)?.join(MARKET_PENDING_FILE))
}

fn load_pending(dsh_home: &Path) -> Result<MarketPendingFile, String> {
    let path = pending_path(dsh_home)?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MarketPendingFile::default())
        }
        Err(error) => return Err(format!("cannot inspect market pending state: {error}")),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("market pending state must not be a symbolic link".to_string())
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err("market pending state is not a regular file".to_string())
        }
        Ok(metadata) if metadata.len() > MARKET_PENDING_MAX_BYTES => {
            return Err("market pending state exceeds 256 KiB".to_string())
        }
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read market pending state: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid market pending state: {e}"))
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
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("market pending state must not be a symbolic link".to_string())
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err("market pending state is not a regular file".to_string())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect market pending state: {error}")),
    }
    let encoded = serde_json::to_vec_pretty(pending)
        .map_err(|e| format!("cannot serialize market pending state: {e}"))?;
    if encoded.len() as u64 > MARKET_PENDING_MAX_BYTES {
        return Err("market pending state exceeds 256 KiB".to_string());
    }
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let temp = path.with_file_name(format!(
        ".market-pending-{}-{}.tmp",
        std::process::id(),
        millis
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("cannot create pending-state temp file: {e}"))?;
    file.write_all(&encoded)
        .map_err(|e| format!("cannot write pending-state temp file: {e}"))?;
    file.write_all(b"\n")
        .map_err(|e| format!("cannot finalize pending-state temp file: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync pending-state temp file: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot chmod pending-state temp file: {e}"))?;
    }
    replace_existing_file(&temp, &path, "market pending state")?;
    Ok(())
}

fn profile_dir(dsh_home: &Path) -> Result<PathBuf, String> {
    checked_real_dir(dsh_home, "DSH_HOME")?;
    let profiles = dsh_home.join("profiles");
    checked_real_dir(&profiles, "profiles directory")?;
    let web = profiles.join("web");
    checked_real_dir(&web, "web profile directory")?;
    let manifest = web.join("package.json");
    let metadata = std::fs::symlink_metadata(&manifest)
        .map_err(|e| format!("cannot inspect web profile package.json: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
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

fn read_profile_manifest(dsh_home: &Path) -> Result<(PathBuf, Value), String> {
    let profile = profile_dir(dsh_home)?;
    let manifest = profile.join("package.json");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read web profile package.json: {e}"))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("web profile package.json is invalid JSON: {e}"))?;
    if !value.is_object() {
        return Err("web profile package.json must contain an object".to_string());
    }
    Ok((profile, value))
}

fn write_profile_manifest(profile: &Path, value: &Value) -> Result<(), String> {
    let path = profile.join("package.json");
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("cannot inspect web profile package.json: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("web profile package.json must be a regular file".to_string());
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("cannot serialize web profile package.json: {e}"))?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let temp = path.with_file_name(format!(
        ".profile-package-{}-{}.tmp",
        std::process::id(),
        millis
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("cannot create web profile package.json temp file: {e}"))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("cannot write web profile package.json: {e}"))?;
    file.write_all(b"\n")
        .map_err(|e| format!("cannot finalize web profile package.json: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync web profile package.json: {e}"))?;
    #[cfg(unix)]
    {
        std::fs::set_permissions(&temp, metadata.permissions())
            .map_err(|e| format!("cannot preserve web profile package.json permissions: {e}"))?;
    }
    replace_existing_file(&temp, &path, "web profile package.json")
}

fn profile_bundles_mut(value: &mut Value) -> Result<&mut Vec<Value>, String> {
    value
        .get_mut("dsh")
        .and_then(Value::as_object_mut)
        .and_then(|dsh| dsh.get_mut("profile"))
        .and_then(Value::as_object_mut)
        .and_then(|profile| profile.get_mut("bundles"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "web profile package.json has no dsh.profile.bundles array".to_string())
}

fn profile_bundles(value: &Value) -> Result<HashSet<&str>, String> {
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

fn profile_dependencies(value: &Value) -> Result<&serde_json::Map<String, Value>, String> {
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
    let (profile, mut manifest) = read_profile_manifest(dsh_home)?;
    let bundles = profile_bundles_mut(&mut manifest)?;
    for bundle in bundles.iter() {
        if bundle.as_str().is_none() {
            return Err("web profile bundles contains a non-string value".to_string());
        }
    }
    bundles.retain(|bundle| bundle.as_str() != Some(candidate.package_name.as_str()));
    write_profile_manifest(&profile, &manifest)
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
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("cannot inspect pnpm lockfile: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("pnpm lockfile must be a regular file".to_string());
    }
    if metadata.len() > PROFILE_LOCK_MAX_BYTES {
        return Err("pnpm lockfile exceeds 4 MiB".to_string());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read pnpm lockfile: {e}"))?;
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

/// Verify all installation facts pnpm materialized. No build script can run
/// in the market pnpm invocation, and normal pending verification requires
/// the profile to stay pre-disabled throughout this check.
fn verify_market_installation(
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

    let installed_manifest = profile
        .join("node_modules")
        .join(&candidate.package_name)
        .join("package.json");
    let text = std::fs::read_to_string(&installed_manifest)
        .map_err(|e| format!("cannot read installed market package: {e}"))?;
    let installed: Value = serde_json::from_str(&text)
        .map_err(|e| format!("installed market package has invalid JSON: {e}"))?;
    if installed.get("name").and_then(Value::as_str) != Some(candidate.package_name.as_str())
        || installed.get("version").and_then(Value::as_str) != Some(candidate.version.as_str())
    {
        return Err(
            "installed market package name/version differs from the reviewed source".to_string(),
        );
    }
    if installed
        .get("dsh")
        .and_then(Value::as_object)
        .and_then(|dsh| dsh.get("bundle"))
        .and_then(Value::as_object)
        .and_then(|bundle| bundle.get("patch"))
        .and_then(Value::as_str)
        .filter(|patch| !patch.is_empty())
        .is_none()
    {
        return Err("installed market package is not a DSH bundle".to_string());
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
    let mut entries = Vec::new();
    for name in dependencies.keys() {
        let version =
            std::fs::read_to_string(profile.join("node_modules").join(name).join("package.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|package| {
                    package
                        .get("version")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "—".to_string());
        let marker = pending
            .plugins
            .get(name)
            .filter(|marker| marker.version == version);
        let (state, slug, entry_revision) = if bundles.contains(name.as_str()) {
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
            name: name.clone(),
            version,
            state,
            slug,
            entry_revision,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

/// The runner state managed by the shell: single-flight flag, app-exit
/// latch, and the live child handle (for cancel and for app-exit cleanup).
pub struct PluginRunner {
    pub busy: AtomicBool,
    pub exiting: AtomicBool,
    pub child: Mutex<Option<PlatformChild>>,
}

impl PluginRunner {
    pub fn new() -> Self {
        PluginRunner {
            busy: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            child: Mutex::new(None),
        }
    }

    /// App-exit cleanup (C1 process-tree guarantee): a running `dsh plugin`
    /// tree is a separate process group / Job Object from the sidecar's
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
        self.exiting.store(true, Ordering::SeqCst);
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
            "#!/bin/sh\nexec \"/opt/node\" \"/opt/harness/node_modules/pnpm/bin/pnpm.cjs\" \"$@\"\n"
        );
        assert_eq!(
            pnpm_shim_cmd("C:\\node.exe", "C:\\harness\\pnpm.cjs"),
            "@echo off\n\"C:\\node.exe\" \"C:\\harness\\pnpm.cjs\" %*\n"
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
            profile.join("pnpm-lock.yaml"),
            format!(
                "lockfileVersion: '9.0'\n\npackages:\n\n  {}@{}:\n    resolution: {{integrity: {}}}\n\nsnapshots:\n",
                candidate.package_name, candidate.version, candidate.integrity
            ),
        )
        .unwrap();
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

        // Simulate a crash after the activation profile write but before the
        // pending marker can be removed.
        let (profile, mut manifest) = read_profile_manifest(&home).expect("profile");
        profile_bundles_mut(&mut manifest)
            .expect("bundle list")
            .push(Value::String(candidate.package_name.clone()));
        write_profile_manifest(&profile, &manifest).expect("simulate activation write");

        assert_eq!(installed_plugins(&home)[0].state, "active");
        activate_market_plugin(&home, &candidate).expect("recovery activation");
        assert!(load_pending(&home)
            .expect("pending state")
            .plugins
            .is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }
}
