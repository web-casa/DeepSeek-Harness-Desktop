//! Shell-side safe transfer for `.dshpreset` archives — the dataelement-grade
//! archive boundary re-implemented in OUR shell. Upstream code is never
//! patched: we validate here and write into upstream's own user preset root
//! (`<dsh_home>/.agent-presets` — upstream's `includeUserRoot` default), so
//! the Harness UI discovers the result through its normal scanRoot.
//!
//! Trust model: a preset runs with the same privileges as the Agent itself
//! (upstream's own words: "carries the same trust as shell access"). The
//! importer must therefore be adversarial-input safe: path traversal,
//! symlinks, zip bombs and secret leakage are rejected or flagged here.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Valid preset id — upstream uses it as a directory name under the root.
pub const PRESET_ID_RE: &str = "^[a-z0-9][a-z0-9-]*$";

const MAX_COMPRESSED: u64 = 16 * 1024 * 1024;
const MAX_UNCOMPRESSED: u64 = 32 * 1024 * 1024;
const MAX_FILE: u64 = 12 * 1024 * 1024;
const MAX_FILES: usize = 512;
const IGNORED_FILES: [&str; 3] = [".DS_Store", "Thumbs.db", "desktop.ini"];
const TEXT_EXTENSIONS: [&str; 17] = [
    ".json", ".jsonc", ".md", ".txt", ".yaml", ".yml", ".toml", ".js", ".jsx", ".ts", ".tsx",
    ".mjs", ".cjs", ".py", ".sh", ".html", ".css",
];
const MAX_SCAN_BYTES: usize = 1024 * 1024;

pub fn user_preset_root(dsh_home: &Path) -> PathBuf {
    dsh_home.join(".agent-presets")
}

/// Root for WRITE/DELETE paths (import/export/delete). Same stance as the
/// DSH_HOME symlink refusal: a symlinked preset root would redirect every
/// removal/install through the link — a user mistake (the dir is inside
/// 0700 DSH_HOME, so only the user can create it) must not turn "delete
/// preset demo" into wiping files in some linked directory. Read-only
/// listing keeps following the link, matching what upstream scanRoot sees.
fn user_preset_root_checked(dsh_home: &Path) -> Result<PathBuf, String> {
    let root = user_preset_root(dsh_home);
    match fs::symlink_metadata(&root) {
        Ok(meta) if meta.file_type().is_symlink() => Err(
            "preset root is a symbolic link — refusing to touch it; resolve the link first"
                .to_string(),
        ),
        Ok(_) => Ok(root),
        // Missing is fine: import creates it on demand.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(root),
        Err(e) => Err(format!("cannot inspect preset root: {e}")),
    }
}

/// Create/protect the user preset root without ever chmodding through a
/// symlink. Kept as the single entry point for both startup initialization and
/// imports so their filesystem posture cannot drift.
pub(crate) fn ensure_user_preset_root(dsh_home: &Path) -> Result<PathBuf, String> {
    let root = user_preset_root_checked(dsh_home)?;
    match fs::symlink_metadata(&root) {
        Ok(meta) if !meta.is_dir() => {
            return Err("preset root is not a directory".to_string());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // This is a direct child of an already-created DSH_HOME. create_dir
            // is atomic and refuses any leaf an attacker races into place.
            fs::create_dir(&root).map_err(|e| format!("cannot create preset root: {e}"))?;
        }
        Err(error) => return Err(format!("cannot inspect preset root: {error}")),
    }
    crate::secure_fs::ensure_private_dir(&root)?;
    Ok(root)
}

/// Health kind of a user preset row, aligned with upstream scanRoot
/// semantics (verified against @deepseek-ai/dsh-agent-presets):
/// - `Broken` — upstream lists the row with a `broken` marker and refuses to
///   mount it. The shell probe deliberately detects only composition
///   missing/unreadable/empty (a 1-byte readability check, never a full
///   load); a readable-but-malformed YAML is STILL caught by upstream's own
///   compositionProblem on the roster — the shell does not reimplement the
///   YAML parser;
/// - `Unsafe` — upstream SKIPS the entry entirely (not a real directory),
///   yet the name stays occupied on disk and no surface shows anything to
///   delete — the shell must surface it;
/// - `Info` — cosmetic only (no preset.yml → no display name; upstream
///   still lists and mounts the preset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetIssueKind {
    Broken,
    Unsafe,
    Info,
}

impl PresetIssueKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PresetIssueKind::Broken => "broken",
            PresetIssueKind::Unsafe => "unsafe",
            PresetIssueKind::Info => "info",
        }
    }
}

/// One roster row's health: healthy presets carry an empty `issues` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetHealth {
    pub id: String,
    pub issues: Vec<(PresetIssueKind, String)>,
}

/// Re-validate the user preset root the way upstream's scanRoot would see
/// it. Every entry whose name is a usable preset id gets a row (directories,
/// symlinks, and regular files alike — the latter two occupy the id while
/// staying invisible to upstream). Entries with names outside the preset-id
/// charset are skipped exactly like upstream skips them.
pub fn validate_user_presets(dsh_home: &Path) -> Vec<PresetHealth> {
    let root = user_preset_root(dsh_home);
    let Ok(entries) = fs::read_dir(&root) else {
        // No root = no presets (upstream scanRoot returns [] on ENOENT).
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !is_valid_preset_id(&name) {
            continue;
        }
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue, // raced away mid-scan; invisible either way
        };
        let mut issues = Vec::new();
        if meta.file_type().is_symlink() {
            issues.push((
                PresetIssueKind::Unsafe,
                "symbolic link — upstream discovery skips it, so the id stays occupied and invisible; delete it".to_string(),
            ));
        } else if !meta.is_dir() {
            issues.push((
                PresetIssueKind::Unsafe,
                "not a directory — upstream discovery skips it, so the id stays occupied and invisible; delete it".to_string(),
            ));
        } else {
            // Health probe, not a full parse: upstream's scanRoot loads and
            // validates the whole composition, but the shell only needs
            // "missing or unreadable" (the plan's broken semantics) — and it
            // runs on every roster refresh, so never load the file. One byte
            // proves it opens and is non-empty (an empty file is broken in
            // upstream's eyes too).
            let probe = fs::File::open(path.join("agent.cordis.yml"))
                .and_then(|mut f| f.read(&mut [0u8; 1]));
            match probe {
                Err(e) => issues.push((
                    PresetIssueKind::Broken,
                    format!(
                        "agent.cordis.yml is missing or unreadable ({e}) — the preset will fail to mount"
                    ),
                )),
                Ok(0) => issues.push((
                    PresetIssueKind::Broken,
                    "agent.cordis.yml is empty — the preset will fail to mount".to_string(),
                )),
                Ok(_) => {
                    if !path.join("preset.yml").is_file() {
                        issues.push((
                            PresetIssueKind::Info,
                            "preset.yml missing — no display name in the roster".to_string(),
                        ));
                    }
                }
            }
        }
        rows.push(PresetHealth { id: name, issues });
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// Delete one user preset by id. Refuses symbolic links (never follow, never
/// remove anything a link merely points at — a malicious link must be
/// resolved manually) and refuses ids outside the preset charset; a regular
/// file occupying the id is removed too.
pub fn delete_preset(id: &str, dsh_home: &Path) -> Result<(), String> {
    if !is_valid_preset_id(id) {
        return Err(format!("invalid preset id {id:?}"));
    }
    let root = user_preset_root_checked(dsh_home)?;
    let target = root.join(id);
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "preset {id} is a symbolic link — refusing to delete; remove it manually"
        )),
        Ok(meta) if meta.is_dir() => {
            fs::remove_dir_all(&target).map_err(|e| format!("cannot remove preset {id}: {e}"))
        }
        Ok(_) => fs::remove_file(&target).map_err(|e| format!("cannot remove entry {id}: {e}")),
        Err(_) => Err(format!("preset {id} not found")),
    }
}

/// True iff the raw entry name can become a path INSIDE the preset dir:
/// no NUL, no backslash, no absolute form (leading `/`, drive letter), and
/// every component is non-empty and not `.`/`..`.
pub fn is_safe_archive_path(name: &str) -> bool {
    if name.is_empty() || name.contains('\0') || name.contains('\\') || name.starts_with('/') {
        return false;
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return false;
    }
    // A single TRAILING slash marks a directory entry (zip tools always emit
    // these); validate the components before it. Interior empty components
    // ("a//b") still fail.
    let core = name.strip_suffix('/').unwrap_or(name);
    if core.is_empty() {
        return false;
    }
    core.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

pub fn is_valid_preset_id(id: &str) -> bool {
    let b = id.as_bytes();
    !id.is_empty()
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Platform-independent "Windows-invalid name" predicate: reserved chars,
/// trailing dot/space, and reserved device names. Applied on EVERY platform
/// at import/export time so a preset accepted on macOS can never fail with
/// an obscure OS error on a Windows machine later.
pub fn windows_invalid_name_reason(name: &str) -> Option<&'static str> {
    let base = name.rsplit('/').next().unwrap_or(name);
    if base
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return Some("invalid characters on Windows");
    }
    if base.ends_with('.') || base.ends_with(' ') {
        return Some("trailing dot/space reserved on Windows");
    }
    let stem = base.split('.').next().unwrap_or(base).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && stem.starts_with("COM") && stem.as_bytes()[3].is_ascii_digit())
        || (stem.len() == 4 && stem.starts_with("LPT") && stem.as_bytes()[3].is_ascii_digit())
    {
        return Some("Windows-reserved device name");
    }
    None
}

fn windows_friendly_check(name: &str) -> Result<(), String> {
    match windows_invalid_name_reason(name) {
        Some(reason) => Err(format!("preset file name is invalid ({reason}): {name}")),
        None => Ok(()),
    }
}

fn extension_of(name: &str) -> &str {
    name.rsplit('/')
        .next()
        .and_then(|base| base.rsplit_once('.'))
        .map(|(_, ext)| ext)
        .unwrap_or("")
}

/// Best-effort secret/absolute-path scan over a text head (≤1 MiB, no NUL,
/// recognized text extension only). Deliberately warning-level: it is a
/// human checkpoint, not the security boundary (the boundary is "only import
/// trusted presets", enforced by the confirmation dialog).
pub fn scan_text_warnings(data: &[u8], ext: &str) -> Vec<&'static str> {
    if data.len() > MAX_SCAN_BYTES || data.contains(&0) || !TEXT_EXTENSIONS.contains(&ext) {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    let has_abs = text.lines().any(|l| {
        l.contains("/Users/")
            || l.contains("/home/")
            || l.contains(":\\")
            || (l.contains("\\\\") && l.contains('\\'))
    });
    if has_abs {
        warnings.push("absolute-paths");
    }
    if text_has_possible_secret(text) {
        warnings.push("possible-secrets");
    }
    warnings
}

fn text_has_possible_secret(text: &str) -> bool {
    let bytes = text.as_bytes();
    // sk- + 16+ of [A-Za-z0-9_-]
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"sk-") {
            let n = bytes[i + 3..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'-')
                .count();
            if n >= 16 {
                return true;
            }
        }
        i += 1;
    }
    // <api_key|apikey|secret|token> [=|:] <12+ non-space non-quote>
    for key in ["api_key", "apikey", "secret", "token"] {
        let mut start = 0usize;
        while let Some(pos) = text[start..].find(key) {
            let rest = &text[start + pos + key.len()..];
            if let Some(rest) = rest.trim_start().strip_prefix(['=', ':']) {
                let rest = rest.trim_start();
                let value_len = rest
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
                    .count();
                if value_len >= 12 {
                    return true;
                }
            }
            start += pos + key.len();
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct ArchivePreview {
    pub id: String,
    pub files: Vec<(String, u64)>,
    pub warnings: Vec<&'static str>,
}

/// Read-only validation pass: quotas, symlink rejection, safe paths, the
/// one-directory top-level shape (id/ with preset.yml + agent.cordis.yml),
/// and the text-head warning scan. Writes nothing.
pub fn inspect_archive(path: &Path) -> Result<ArchivePreview, String> {
    let meta = fs::metadata(path).map_err(|e| format!("cannot stat archive: {e}"))?;
    if meta.len() > MAX_COMPRESSED {
        return Err(format!(
            "archive too large: {} bytes (max {MAX_COMPRESSED})",
            meta.len()
        ));
    }
    let file = fs::File::open(path).map_err(|e| format!("cannot open archive: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a zip archive: {e}"))?;
    if zip.len() > MAX_FILES {
        return Err(format!("too many entries: {} (max {MAX_FILES})", zip.len()));
    }

    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("cannot read entry {i}: {e}"))?;
        let raw_name = entry.name().to_string();
        if IGNORED_FILES.contains(&raw_name.as_str())
            || raw_name
                .rsplit('/')
                .next()
                .is_some_and(|b| IGNORED_FILES.contains(&b))
        {
            continue;
        }
        if entry.is_symlink() {
            return Err(format!("preset contains a symbolic link: {raw_name}"));
        }
        if !is_safe_archive_path(&raw_name) {
            return Err(format!("preset contains an unsafe path: {raw_name}"));
        }
        windows_friendly_check(&raw_name)?;
        if entry.size() > MAX_FILE {
            return Err(format!("preset file too large: {raw_name}"));
        }
        total += entry.size();
        if total > MAX_UNCOMPRESSED {
            return Err(format!("preset expands beyond {MAX_UNCOMPRESSED} bytes"));
        }
        if !entry.is_dir() && entry.size() > 0 && entry.size() <= MAX_SCAN_BYTES as u64 {
            let ext = extension_of(&raw_name);
            if TEXT_EXTENSIONS.contains(&ext) {
                let mut buf = vec![0u8; entry.size() as usize];
                entry
                    .read_exact(&mut buf)
                    .map_err(|e| format!("cannot read {raw_name}: {e}"))?;
                warnings.extend(scan_text_warnings(&buf, ext));
            }
        }
        files.push((raw_name, entry.size()));
    }

    let id = derive_preset_id(&files)?;
    let base = format!("{id}/");
    let has_composition = files
        .iter()
        .any(|(n, _)| n == &format!("{base}agent.cordis.yml"));
    let has_metadata = files.iter().any(|(n, _)| n == &format!("{base}preset.yml"));
    if !has_composition {
        return Err("preset is missing agent.cordis.yml".to_string());
    }
    if !has_metadata {
        return Err("preset is missing preset.yml".to_string());
    }
    Ok(ArchivePreview {
        id,
        files,
        warnings,
    })
}

/// The archive's top level must be EXACTLY one directory named by a valid
/// preset id; everything else must live under it.
fn derive_preset_id(files: &[(String, u64)]) -> Result<String, String> {
    let mut top: Option<&str> = None;
    for (name, _) in files {
        let (head, rest) = name.split_once('/').unwrap_or((name.as_str(), ""));
        if rest.is_empty() {
            // Only a bare "<id>/" directory entry is legal here; a bare
            // top-level FILE would sit outside the preset directory.
            if !name.ends_with('/') {
                return Err(format!("entry outside the preset directory: {name}"));
            }
            match top {
                None => top = Some(head),
                Some(t) if t == head => {}
                Some(t) => {
                    return Err(format!(
                        "archive must contain exactly one preset directory (found {t} and {head})"
                    ));
                }
            }
            continue;
        }
        match top {
            None => top = Some(head),
            Some(t) if t == head => {}
            Some(t) => {
                return Err(format!(
                    "archive must contain exactly one preset directory (found {t} and {head})"
                ));
            }
        }
    }
    let id = top.ok_or("archive is empty")?;
    if !is_valid_preset_id(id) {
        return Err(format!(
            "invalid preset id {id:?} (must match {PRESET_ID_RE})"
        ));
    }
    Ok(id.to_string())
}

/// Two-phase install, phase 2: re-validate, extract into a staging dir under
/// the user root with BOUNDED reads (declared sizes can lie), tighten modes
/// to upstream's 0700/0600 posture, then atomically rename into place.
/// Conflicts never overwrite.
pub fn install_archive(path: &Path, dsh_home: &Path) -> Result<String, String> {
    let preview = inspect_archive(path)?;
    let root = ensure_user_preset_root(dsh_home)?;
    // Sweep stale staging dirs from crashed/killed runs: names start with
    // ".tmp-" and match neither the preset id regex nor upstream's scan.
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(".tmp-") {
                let path = entry.path();
                match fs::symlink_metadata(&path) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        let _ = fs::remove_file(path);
                    }
                    Ok(meta) if meta.is_dir() => {
                        let _ = fs::remove_dir_all(path);
                    }
                    _ => {}
                }
            }
        }
    }
    let target = root.join(&preview.id);
    match fs::symlink_metadata(&target) {
        Ok(_) => {
            return Err(format!(
                "preset {} already exists — not overwriting",
                preview.id
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect preset destination: {error}")),
    }
    let staging = root.join(format!(
        ".tmp-{}-{}-{}",
        preview.id,
        std::process::id(),
        crate::secure_fs::random_suffix()?
    ));

    if let Err(e) = extract_bounded(path, &preview.id, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    match fs::rename(&staging, &target) {
        Ok(()) => Ok(preview.id),
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            Err(if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("preset {} already exists — not overwriting", preview.id)
            } else {
                format!("cannot move preset into place: {e}")
            })
        }
    }
}

/// Extract every entry (leading `id/` stripped) into `staging`, enforcing the
/// real decompressed byte counts and the containment invariant at WRITE time.
/// The zip crate enforces its own per-entry limits from the headers it trusts
/// (a "lying size" fixture cannot be constructed through its public API — the
/// crate truncates internally); our counters are defense-in-depth on top.
fn extract_bounded(path: &Path, id: &str, staging: &Path) -> Result<(), String> {
    let file = fs::File::open(path).map_err(|e| format!("cannot open archive: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a zip archive: {e}"))?;
    let mut total = 0u64;
    if zip.len() > MAX_FILES {
        return Err(format!("too many entries: {} (max {MAX_FILES})", zip.len()));
    }
    fs::create_dir(staging).map_err(|e| format!("cannot create staging: {e}"))?;

    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| format!("cannot read entry {i}: {e}"))?;
        let raw_name = entry.name().to_string();
        if raw_name
            .rsplit('/')
            .next()
            .is_some_and(|b| IGNORED_FILES.contains(&b))
        {
            continue;
        }
        if entry.is_symlink() {
            return Err(format!("preset contains a symbolic link: {raw_name}"));
        }
        if !is_safe_archive_path(&raw_name) {
            return Err(format!("preset contains an unsafe path: {raw_name}"));
        }
        windows_friendly_check(&raw_name)?;
        let mut parts = raw_name.split('/');
        if parts.next() != Some(id) {
            return Err(format!(
                "preset entry outside the preset directory: {raw_name}"
            ));
        }
        let rel: PathBuf = parts.collect(); // staging IS the id directory
        if rel.as_os_str().is_empty() {
            continue; // the directory entry itself
        }
        let out = staging.join(&rel);
        if !out.starts_with(staging) {
            return Err(format!(
                "preset entry escapes the preset directory: {raw_name}"
            ));
        }
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| format!("cannot create dir: {e}"))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create dir: {e}"))?;
        }
        let mut dest = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .map_err(|e| format!("cannot create unique preset file {raw_name}: {e}"))?;
        let mut copied = 0u64;
        let mut buf = [0u8; 64 * 1024];
        let mut reader = entry.take(MAX_FILE + 1);
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("cannot extract {raw_name}: {e}"))?;
            if n == 0 {
                break;
            }
            copied += n as u64;
            if copied > MAX_FILE {
                return Err(format!("preset file exceeds {MAX_FILE} bytes: {raw_name}"));
            }
            total += n as u64;
            if total > MAX_UNCOMPRESSED {
                return Err(format!("preset expands beyond {MAX_UNCOMPRESSED} bytes"));
            }
            dest.write_all(&buf[..n])
                .map_err(|e| format!("cannot write {raw_name}: {e}"))?;
        }
    }
    tighten_modes(staging)?;
    Ok(())
}

/// Match upstream's posture: preset dirs 0700, files 0600 (owner-exec kept).
fn tighten_modes(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fn walk(d: &Path) -> Result<(), String> {
            fs::set_permissions(d, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
            for entry in fs::read_dir(d).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                let meta = entry.metadata().map_err(|e| e.to_string())?;
                if meta.is_dir() {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                        .map_err(|e| e.to_string())?;
                    walk(&path)?;
                } else {
                    let mut mode = 0o600;
                    if meta.permissions().mode() & 0o100 != 0 {
                        mode |= 0o100;
                    }
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                        .map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        }
        walk(dir)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Export a user-authored preset as a `.dshpreset` zip. System presets are
/// out of scope (read-only, shipped with the bundle).
pub fn export_preset(id: &str, dsh_home: &Path, dest: &Path) -> Result<(), String> {
    if !is_valid_preset_id(id) {
        return Err(format!("invalid preset id {id:?}"));
    }
    let dir = user_preset_root_checked(dsh_home)?.join(id);
    // Same stance as the import side: never follow a symlink that happens
    // to wear a valid preset id (list_user_presets flags those as unsafe).
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "preset {id} is a symbolic link — refusing to export"
            ));
        }
        Ok(_) => {}
        Err(_) => return Err(format!("preset {id} not found")),
    }
    if !dir.is_dir() {
        return Err(format!("preset {id} not found"));
    }
    if !dir.join("agent.cordis.yml").is_file() || !dir.join("preset.yml").is_file() {
        return Err(format!("preset {id} is incomplete"));
    }

    crate::secure_fs::check_regular_or_missing(dest)?;
    let parent = dest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let source = fs::canonicalize(&dir)
        .map_err(|e| format!("cannot resolve preset source directory: {e}"))?;
    let destination_parent = fs::canonicalize(parent)
        .map_err(|e| format!("cannot resolve preset export directory: {e}"))?;
    if destination_parent.starts_with(&source) {
        return Err("preset cannot be exported inside its own source directory".to_string());
    }
    let temp = crate::secure_fs::sibling_temp(dest, "preset-export")?;

    fn walk(
        zip: &mut zip::ZipWriter<fs::File>,
        base: &Path,
        dir: &Path,
        id: &str,
        options: zip::write::SimpleFileOptions,
        count: &mut usize,
        total: &mut u64,
    ) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            // DirEntry::metadata follows symlinks. Inspect the directory entry
            // itself first so a linked directory cannot make the exporter
            // recurse outside the preset and disclose unrelated files.
            let file_type = entry.file_type().map_err(|e| e.to_string())?;
            if file_type.is_symlink() {
                return Err("preset contains a symbolic link".to_string());
            }
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            let rel = path
                .strip_prefix(base)
                .map_err(|_| "preset path escaped its directory".to_string())?;
            let name = format!("{id}/{}", rel.to_string_lossy().replace('\\', "/"));
            *count += 1;
            if *count > MAX_FILES {
                return Err("too many files to export".to_string());
            }
            if meta.is_dir() {
                zip.add_directory(name, options)
                    .map_err(|e| e.to_string())?;
                walk(zip, base, &path, id, options, count, total)?;
            } else {
                *total += meta.len();
                if meta.len() > MAX_FILE || *total > MAX_UNCOMPRESSED {
                    return Err("preset exceeds export limits".to_string());
                }
                zip.start_file(name, options).map_err(|e| e.to_string())?;
                let mut src = crate::secure_fs::open_regular_read(&path)?;
                std::io::copy(&mut src, zip).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    let result = (|| {
        let file = crate::secure_fs::create_private_new(&temp)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let mut total = 0u64;
        let mut count = 0usize;
        walk(&mut zip, &dir, &dir, id, options, &mut count, &mut total)?;
        let file = zip.finish().map_err(|e| e.to_string())?;
        file.sync_all()
            .map_err(|e| format!("cannot sync preset archive: {e}"))?;
        drop(file);
        crate::secure_fs::replace_file(&temp, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dsd-preset-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Entry {
        name: String,
        data: Vec<u8>,
        symlink: Option<String>,
    }

    fn file(name: &str, data: &[u8]) -> Entry {
        Entry {
            name: name.to_string(),
            data: data.to_vec(),
            symlink: None,
        }
    }

    fn write_archive(entries: &[Entry]) -> PathBuf {
        let path = temp_dir("zip").join("fixture.dshpreset");
        let f = fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        for e in entries {
            match &e.symlink {
                Some(target) => {
                    zip.add_symlink(&e.name, target, opts).unwrap();
                }
                None => {
                    zip.start_file(&e.name, opts).unwrap();
                    zip.write_all(&e.data).unwrap();
                }
            }
        }
        zip.finish().unwrap();
        path
    }

    fn benign() -> Vec<Entry> {
        vec![
            file(
                "my-preset/agent.cordis.yml",
                b"# composition\nmodel: standard\n",
            ),
            file(
                "my-preset/preset.yml",
                b"name: demo\ndescription: x\norder: 1\n",
            ),
        ]
    }

    #[test]
    fn safe_path_table() {
        assert!(is_safe_archive_path("my-preset/agent.cordis.yml"));
        assert!(is_safe_archive_path("my-preset/skills/a b.md"));
        assert!(!is_safe_archive_path(""));
        assert!(!is_safe_archive_path("../evil"));
        assert!(!is_safe_archive_path("my-preset/../evil"));
        assert!(!is_safe_archive_path("my-preset/./x"));
        assert!(!is_safe_archive_path("/etc/passwd"));
        assert!(!is_safe_archive_path("C:/evil"));
        assert!(!is_safe_archive_path("a\\b"));
        assert!(!is_safe_archive_path("a/\0b"));
    }

    #[test]
    fn warnings_scan_table() {
        let sk = b"x sk-abcdefghijklmnop123 y";
        let key = b"api_key: abcdefghijklmnop";
        let abs = b"path: /Users/alice/secret";
        let win_abs = b"run: C:\\evil\\x.exe";
        assert_eq!(scan_text_warnings(sk, ".md"), vec!["possible-secrets"]);
        assert_eq!(scan_text_warnings(key, ".yml"), vec!["possible-secrets"]);
        assert_eq!(scan_text_warnings(abs, ".yml"), vec!["absolute-paths"]);
        assert_eq!(scan_text_warnings(win_abs, ".sh"), vec!["absolute-paths"]);
        // Non-text extension: skipped.
        assert!(scan_text_warnings(sk, ".png").is_empty());
        // Too-large and NUL payloads: skipped, never panics.
        assert!(scan_text_warnings(&vec![0u8; MAX_SCAN_BYTES + 1], ".md").is_empty());
        assert!(scan_text_warnings(b"a\0sk-abcdefghijklmnop123", ".md").is_empty());
    }

    #[test]
    fn windows_name_table() {
        assert_eq!(windows_invalid_name_reason("id/skills/a.md"), None);
        assert_eq!(
            windows_invalid_name_reason("id/a:b.md"),
            Some("invalid characters on Windows")
        );
        assert_eq!(
            windows_invalid_name_reason("id/bad."),
            Some("trailing dot/space reserved on Windows")
        );
        assert_eq!(
            windows_invalid_name_reason("id/bad "),
            Some("trailing dot/space reserved on Windows")
        );
        assert_eq!(
            windows_invalid_name_reason("id/NUL"),
            Some("Windows-reserved device name")
        );
        assert_eq!(
            windows_invalid_name_reason("id/COM1.txt"),
            Some("Windows-reserved device name")
        );
        assert_eq!(windows_invalid_name_reason("id/console.md"), None);
    }

    #[test]
    fn preset_id_table() {
        assert!(is_valid_preset_id("my-preset"));
        assert!(is_valid_preset_id("9lives"));
        assert!(!is_valid_preset_id("-lead"));
        assert!(!is_valid_preset_id("MyPreset"));
        assert!(!is_valid_preset_id("a/b"));
        assert!(!is_valid_preset_id(""));
    }

    #[test]
    fn benign_archive_installs_and_is_discovered_layout() {
        let archive = write_archive(&benign());
        let dsh_home = temp_dir("home");
        let preview = inspect_archive(&archive).expect("inspect");
        assert_eq!(preview.id, "my-preset");
        assert!(preview.warnings.is_empty());
        let id = install_archive(&archive, &dsh_home).expect("install");
        assert_eq!(id, "my-preset");
        let root = user_preset_root(&dsh_home).join("my-preset");
        assert!(root.join("preset.yml").is_file());
        assert!(root.join("agent.cordis.yml").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&root).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "preset dir must be 0700");
            let file_mode = fs::metadata(root.join("preset.yml"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(file_mode & 0o777, 0o600, "preset files must be 0600");
        }
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_file(&archive).unwrap();
    }

    #[test]
    fn traversal_entry_is_rejected() {
        let archive = write_archive(&[
            file("../evil", b"x"),
            file("my-preset/agent.cordis.yml", b"x"),
            file("my-preset/preset.yml", b"x"),
        ]);
        assert!(inspect_archive(&archive).is_err());
        let dsh_home = temp_dir("home2");
        assert!(install_archive(&archive, &dsh_home).is_err());
        // Nothing escaped: the user root contains nothing but maybe tmp residue-free
        let root = user_preset_root(&dsh_home);
        let leftovers = fs::read_dir(&root)
            .map(|rd| rd.flatten().count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "no preset may remain after a rejected import");
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_file(&archive).unwrap();
    }

    #[test]
    fn symlink_entry_is_rejected() {
        let archive = write_archive(&[
            Entry {
                name: "my-preset/link".to_string(),
                data: Vec::new(),
                symlink: Some("/etc/passwd".to_string()),
            },
            file("my-preset/agent.cordis.yml", b"x"),
            file("my-preset/preset.yml", b"x"),
        ]);
        let err = inspect_archive(&archive).unwrap_err();
        assert!(err.contains("symbolic link"), "unexpected error: {err}");
        fs::remove_file(&archive).unwrap();
    }

    #[test]
    fn shape_violations_are_rejected() {
        // Two top-level directories.
        let a = write_archive(&[
            file("a/agent.cordis.yml", b"x"),
            file("a/preset.yml", b"x"),
            file("b/preset.yml", b"x"),
        ]);
        assert!(inspect_archive(&a).unwrap_err().contains("exactly one"));
        fs::remove_file(&a).unwrap();
        // Missing composition.
        let b = write_archive(&[file("p/preset.yml", b"x")]);
        assert!(inspect_archive(&b)
            .unwrap_err()
            .contains("agent.cordis.yml"));
        fs::remove_file(&b).unwrap();
        // Invalid id.
        let c = write_archive(&[
            file("UPPER/agent.cordis.yml", b"x"),
            file("UPPER/preset.yml", b"x"),
        ]);
        assert!(inspect_archive(&c)
            .unwrap_err()
            .contains("invalid preset id"));
        fs::remove_file(&c).unwrap();
    }

    #[test]
    fn declared_oversize_is_rejected() {
        let archive = write_archive(&[
            Entry {
                name: "big/blob.bin".to_string(),
                data: vec![0u8; (MAX_FILE + 1) as usize],
                symlink: None,
            },
            file("big/agent.cordis.yml", b"x"),
            file("big/preset.yml", b"x"),
        ]);
        assert!(inspect_archive(&archive).unwrap_err().contains("too large"));
        fs::remove_file(&archive).unwrap();
    }

    #[test]
    fn double_install_conflicts_without_overwrite() {
        let archive = write_archive(&benign());
        let dsh_home = temp_dir("home3");
        install_archive(&archive, &dsh_home).unwrap();
        let err = install_archive(&archive, &dsh_home).unwrap_err();
        assert!(err.contains("already exists"), "unexpected error: {err}");
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_file(&archive).unwrap();
    }

    #[test]
    fn conflicting_archive_paths_are_rejected_without_partial_install() {
        let archive = write_archive(&[
            file("my-preset/conflict", b"file"),
            file("my-preset/conflict/child.txt", b"child"),
            file("my-preset/agent.cordis.yml", b"composition"),
            file("my-preset/preset.yml", b"name: demo\n"),
        ]);
        let dsh_home = temp_dir("conflicting-paths");
        let error = install_archive(&archive, &dsh_home).unwrap_err();
        assert!(error.contains("cannot create dir"), "{error}");
        assert!(!user_preset_root(&dsh_home).join("my-preset").exists());
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_file(&archive).unwrap();
    }

    #[test]
    fn export_roundtrips_through_inspect() {
        let archive = write_archive(&benign());
        let dsh_home = temp_dir("home4");
        install_archive(&archive, &dsh_home).unwrap();
        let out = temp_dir("out").join("roundtrip.dshpreset");
        export_preset("my-preset", &dsh_home, &out).unwrap();
        let preview = inspect_archive(&out).expect("exported archive must pass inspect");
        assert_eq!(preview.id, "my-preset");
        assert!(preview
            .files
            .iter()
            .any(|(n, _)| n == "my-preset/agent.cordis.yml"));
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_file(&archive).unwrap();
        fs::remove_file(&out).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn export_refuses_symlink_destination_without_touching_target() {
        use std::os::unix::fs::symlink;
        let archive = write_archive(&benign());
        let dsh_home = temp_dir("export-link-home");
        install_archive(&archive, &dsh_home).unwrap();
        let out_dir = temp_dir("export-link-out");
        let target = out_dir.join("keep.txt");
        fs::write(&target, b"keep").unwrap();
        let destination = out_dir.join("preset.dshpreset");
        symlink(&target, &destination).unwrap();
        let error = export_preset("my-preset", &dsh_home, &destination).unwrap_err();
        assert!(error.contains("regular file"), "{error}");
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_dir_all(&out_dir).unwrap();
        fs::remove_file(&archive).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn export_refuses_nested_symlink_without_disclosing_target() {
        use std::os::unix::fs::symlink;
        let archive = write_archive(&benign());
        let dsh_home = temp_dir("export-nested-link-home");
        install_archive(&archive, &dsh_home).unwrap();
        let outside = temp_dir("export-nested-link-outside");
        fs::write(outside.join("secret.txt"), b"must not be archived").unwrap();
        symlink(
            &outside,
            user_preset_root(&dsh_home).join("my-preset/linked"),
        )
        .unwrap();
        let out_dir = temp_dir("export-nested-link-output");
        let destination = out_dir.join("preset.dshpreset");

        let error = export_preset("my-preset", &dsh_home, &destination).unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
        assert!(!destination.exists());

        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_dir_all(&outside).unwrap();
        fs::remove_dir_all(&out_dir).unwrap();
        fs::remove_file(&archive).unwrap();
    }

    // ---- P2: preset-root health re-validation ---------------------------

    fn health_kinds(id: &str, rows: &[PresetHealth]) -> Vec<PresetIssueKind> {
        rows.iter()
            .find(|r| r.id == id)
            .map(|r| r.issues.iter().map(|(k, _)| *k).collect())
            .unwrap_or_default()
    }

    #[test]
    fn validate_user_presets_table() {
        let dsh_home = temp_dir("health");
        let root = user_preset_root(&dsh_home);
        fs::create_dir_all(&root).unwrap();

        // Healthy: composition + metadata.
        fs::create_dir_all(root.join("demo")).unwrap();
        fs::write(root.join("demo").join("agent.cordis.yml"), "[]\n").unwrap();
        fs::write(root.join("demo").join("preset.yml"), "name: demo\n").unwrap();

        // Broken: composition missing.
        fs::create_dir_all(root.join("missing")).unwrap();
        fs::write(root.join("missing").join("preset.yml"), "name: x\n").unwrap();

        // Broken: composition path exists but is not a readable file.
        fs::create_dir_all(root.join("unreadable")).unwrap();
        fs::create_dir_all(root.join("unreadable").join("agent.cordis.yml")).unwrap();

        // Broken: composition exists but is empty (upstream treats it as
        // broken too).
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("empty").join("agent.cordis.yml"), "").unwrap();

        // Info: composition fine, metadata missing.
        fs::create_dir_all(root.join("nometa")).unwrap();
        fs::write(root.join("nometa").join("agent.cordis.yml"), "[]\n").unwrap();

        // Unsafe: a regular file occupying a preset id (invisible upstream).
        fs::write(root.join("afile"), "oops").unwrap();

        // Invalid id: skipped like upstream skips it.
        fs::create_dir_all(root.join("UPPER")).unwrap();
        fs::write(root.join("UPPER").join("agent.cordis.yml"), "[]\n").unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();

        let rows = validate_user_presets(&dsh_home);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            ["afile", "demo", "empty", "missing", "nometa", "unreadable"]
        );
        assert!(health_kinds("demo", &rows).is_empty());
        assert_eq!(health_kinds("missing", &rows), [PresetIssueKind::Broken]);
        assert_eq!(health_kinds("unreadable", &rows), [PresetIssueKind::Broken]);
        assert_eq!(health_kinds("empty", &rows), [PresetIssueKind::Broken]);
        assert_eq!(health_kinds("nometa", &rows), [PresetIssueKind::Info]);
        assert_eq!(health_kinds("afile", &rows), [PresetIssueKind::Unsafe]);
        fs::remove_dir_all(&dsh_home).unwrap();
    }

    #[test]
    fn validate_missing_root_is_empty() {
        let dsh_home = temp_dir("noroot");
        assert!(validate_user_presets(&dsh_home).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn validate_flags_symlinks_unsafe() {
        use std::os::unix::fs::symlink;
        let dsh_home = temp_dir("symlink");
        let root = user_preset_root(&dsh_home);
        fs::create_dir_all(&root).unwrap();
        let target = temp_dir("link-target");
        symlink(&target, root.join("linked")).unwrap();
        let rows = validate_user_presets(&dsh_home);
        assert_eq!(health_kinds("linked", &rows), [PresetIssueKind::Unsafe]);
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_dir_all(&target).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn validate_skips_non_utf8_entry_names() {
        use std::os::unix::ffi::OsStrExt;
        let dsh_home = temp_dir("nonutf8");
        let root = user_preset_root(&dsh_home);
        fs::create_dir_all(&root).unwrap();
        let weird = std::ffi::OsStr::from_bytes(b"bad\xff\xfe");
        fs::create_dir_all(root.join(weird)).unwrap();
        let rows = validate_user_presets(&dsh_home);
        assert!(rows.is_empty(), "non-UTF-8 names must be skipped: {rows:?}");
        fs::remove_dir_all(&dsh_home).unwrap();
    }

    #[test]
    fn delete_preset_table() {
        let dsh_home = temp_dir("delete");
        let root = user_preset_root(&dsh_home);
        fs::create_dir_all(root.join("demo")).unwrap();
        fs::write(root.join("demo").join("agent.cordis.yml"), "[]\n").unwrap();
        fs::write(root.join("demo").join("preset.yml"), "name: demo\n").unwrap();

        assert!(delete_preset("demo", &dsh_home).is_ok());
        assert!(!root.join("demo").exists());
        assert!(delete_preset("demo", &dsh_home).is_err()); // gone now

        assert!(delete_preset("BAD!id", &dsh_home).is_err());
        assert!(delete_preset("UPPER", &dsh_home).is_err()); // regex mismatch

        // A regular file occupying the id is removable (it blocks the id).
        fs::write(root.join("afile"), "x").unwrap();
        assert!(delete_preset("afile", &dsh_home).is_ok());
        assert!(!root.join("afile").exists());
        fs::remove_dir_all(&dsh_home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn delete_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let dsh_home = temp_dir("del-link");
        let root = user_preset_root(&dsh_home);
        fs::create_dir_all(&root).unwrap();
        let target = temp_dir("del-link-target");
        fs::write(target.join("keep.txt"), "keep").unwrap();
        symlink(&target, root.join("linked")).unwrap();
        let err = delete_preset("linked", &dsh_home).unwrap_err();
        assert!(err.contains("symbolic link"), "{err}");
        // The link and its target are both untouched.
        assert!(root.join("linked").exists());
        assert!(target.join("keep.txt").exists());
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_dir_all(&target).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_paths_refuse_a_symlinked_root() {
        use std::os::unix::fs::symlink;
        let dsh_home = temp_dir("root-link");
        let target = temp_dir("root-link-target");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, user_preset_root(&dsh_home)).unwrap();
        let err = delete_preset("demo", &dsh_home).unwrap_err();
        assert!(err.contains("preset root is a symbolic link"), "{err}");
        let err = export_preset("demo", &dsh_home, &dsh_home.join("out.zip")).unwrap_err();
        assert!(err.contains("preset root is a symbolic link"), "{err}");
        // install_archive refuses too — but inspect runs first; feed a valid
        // archive to reach the root check.
        let archive = write_archive(&benign());
        let err = install_archive(&archive, &dsh_home).unwrap_err();
        assert!(err.contains("preset root is a symbolic link"), "{err}");
        assert!(fs::read_dir(&target).unwrap().next().is_none());
        fs::remove_dir_all(&dsh_home).unwrap();
        fs::remove_dir_all(&target).unwrap();
        fs::remove_file(&archive).unwrap();
    }
}
