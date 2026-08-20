//! Small, dependency-free primitives for Desktop-owned private state.
//!
//! Callers must keep files below an already-checked private directory. Every
//! leaf is rejected when it is a symlink or Windows reparse point, writes use
//! private permissions, and
//! replacement is staged beside the destination so a crash cannot expose a
//! partially written JSON document.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;

pub(crate) fn is_symlink_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn ensure_private_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if is_symlink_or_reparse(&meta) || !meta.is_dir() {
                return Err(format!(
                    "private state path is not a real directory: {}",
                    path.display()
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|e| {
                format!(
                    "cannot create private state directory {}: {e}",
                    path.display()
                )
            })?;
            let meta = fs::symlink_metadata(path).map_err(|e| {
                format!(
                    "cannot inspect private state directory {}: {e}",
                    path.display()
                )
            })?;
            if is_symlink_or_reparse(&meta) || !meta.is_dir() {
                return Err(format!(
                    "private state path is not a real directory: {}",
                    path.display()
                ));
            }
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect private state directory {}: {error}",
                path.display()
            ));
        }
    }
    #[cfg(unix)]
    {
        // Open the directory itself without following the final component and
        // chmod the handle. A path-based chmod after symlink_metadata leaves a
        // race where another local process can swap the leaf for a symlink.
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
        let directory = options.open(path).map_err(|e| {
            format!(
                "cannot securely open private state directory {}: {e}",
                path.display()
            )
        })?;
        if !directory
            .metadata()
            .map_err(|e| format!("cannot inspect private directory handle: {e}"))?
            .is_dir()
        {
            return Err(format!(
                "private state path is not a real directory: {}",
                path.display()
            ));
        }
        directory
            .set_permissions(fs::Permissions::from_mode(PRIVATE_DIR_MODE))
            .map_err(|e| {
                format!(
                    "cannot protect private state directory {}: {e}",
                    path.display()
                )
            })?;
    }
    Ok(())
}

fn no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

fn validate_regular_handle(file: &File, path: &Path) -> Result<fs::Metadata, String> {
    let metadata = file
        .metadata()
        .map_err(|e| format!("cannot inspect open file {}: {e}", path.display()))?;
    if is_symlink_or_reparse(&metadata) || !metadata.is_file() {
        return Err(format!(
            "private state leaf is not a regular file: {}",
            path.display()
        ));
    }
    Ok(metadata)
}

/// Open an existing regular file while refusing a symlink/reparse-point leaf.
/// Callers that impose a byte limit must still read through a bounded adapter,
/// because the file can grow after the handle is opened.
pub fn open_regular_read(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let file = options
        .open(path)
        .map_err(|e| format!("cannot securely open file {}: {e}", path.display()))?;
    validate_regular_handle(&file, path)?;
    Ok(file)
}

pub fn check_regular_or_missing(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if is_symlink_or_reparse(&meta) || !meta.is_file() => Err(format!(
            "private state leaf is not a regular file: {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot inspect private state leaf {}: {error}",
            path.display()
        )),
    }
}

pub fn create_private_new(path: &Path) -> Result<File, String> {
    check_regular_or_missing(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(PRIVATE_FILE_MODE);
    options
        .open(path)
        .map_err(|e| format!("cannot create private file {}: {e}", path.display()))
}

pub fn open_private_append(path: &Path) -> Result<File, String> {
    check_regular_or_missing(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).append(true);
    no_follow(&mut options);
    #[cfg(unix)]
    options.mode(PRIVATE_FILE_MODE);
    let file = options
        .open(path)
        .map_err(|e| format!("cannot open private file {}: {e}", path.display()))?;
    validate_regular_handle(&file, path)?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .map_err(|e| format!("cannot protect private file {}: {e}", path.display()))?;
    Ok(file)
}

pub fn read_bounded(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect private file {}: {error}",
                path.display()
            ))
        }
    };
    if is_symlink_or_reparse(&meta) || !meta.is_file() {
        return Err(format!(
            "private state leaf is not a regular file: {}",
            path.display()
        ));
    }
    if meta.len() > max_bytes {
        return Err(format!(
            "private file exceeds {max_bytes} byte limit: {}",
            path.display()
        ));
    }
    let file = open_regular_read(path)?;
    let opened_meta = file
        .metadata()
        .map_err(|e| format!("cannot inspect private file {}: {e}", path.display()))?;
    if opened_meta.len() > max_bytes {
        return Err(format!(
            "private file exceeds {max_bytes} byte limit: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(opened_meta.len() as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read private file {}: {e}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "private file changed beyond {max_bytes} byte limit: {}",
            path.display()
        ));
    }
    Ok(Some(bytes))
}

pub fn random_suffix() -> Result<String, String> {
    let mut bytes = [0_u8; 12];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| format!("cannot generate private file id: {e}"))?;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}")
            .map_err(|e| format!("cannot format private file id: {e}"))?;
    }
    Ok(output)
}

pub fn atomic_write(path: &Path, bytes: &[u8], max_bytes: usize) -> Result<(), String> {
    if bytes.len() > max_bytes {
        return Err(format!(
            "private state payload exceeds {max_bytes} byte limit"
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| "private state path has no parent".to_string())?;
    ensure_private_dir(parent)?;
    check_regular_or_missing(path)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let temp = parent.join(format!(".{name}.{}.tmp", random_suffix()?));
    let result = (|| {
        let mut file = create_private_new(&temp)?;
        file.write_all(bytes)
            .map_err(|e| format!("cannot write private state {}: {e}", path.display()))?;
        file.sync_all()
            .map_err(|e| format!("cannot sync private state {}: {e}", path.display()))?;
        drop(file);
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
pub fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    check_regular_or_missing(destination)?;
    fs::rename(source, destination).map_err(|e| {
        format!(
            "cannot publish private file {} to {}: {e}",
            source.display(),
            destination.display()
        )
    })
}

#[cfg(windows)]
pub fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    check_regular_or_missing(destination)?;
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both buffers are NUL-terminated and live for the duration of
    // the call. The destination was checked above and the source is an
    // application-created file in the same directory.
    let ok = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(format!(
            "cannot publish private file {} to {}: {}",
            source.display(),
            destination.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

pub fn sibling_temp(destination: &Path, purpose: &str) -> Result<PathBuf, String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "destination has no parent".to_string())?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("diagnostics.zip");
    Ok(parent.join(format!(".{name}.{purpose}.{}.tmp", random_suffix()?)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshd-secure-fs-{name}-{}",
            random_suffix().unwrap()
        ))
    }

    #[test]
    fn atomic_write_replaces_complete_payload() {
        let root = test_dir("atomic");
        ensure_private_dir(&root).unwrap();
        let path = root.join("state.json");
        atomic_write(&path, b"one", 16).unwrap();
        atomic_write(&path, b"two", 16).unwrap();
        assert_eq!(read_bounded(&path, 16).unwrap(), Some(b"two".to_vec()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_read_rejects_large_file() {
        let root = test_dir("bounded");
        ensure_private_dir(&root).unwrap();
        let path = root.join("large");
        atomic_write(&path, b"12345", 8).unwrap();
        assert!(read_bounded(&path, 4).unwrap_err().contains("exceeds"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_leaf_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = test_dir("symlink");
        ensure_private_dir(&root).unwrap();
        let target = root.join("target");
        fs::write(&target, b"secret").unwrap();
        let link = root.join("state.json");
        symlink(&target, &link).unwrap();
        assert!(atomic_write(&link, b"replacement", 64).is_err());
        assert!(read_bounded(&link, 64).is_err());
        assert!(open_private_append(&link).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"secret");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_directory_is_rejected_without_chmodding_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = test_dir("directory-symlink");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.join("state");
        symlink(&target, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn junction_directory_is_rejected() {
        let root = test_dir("junction");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target");
        let junction = root.join("junction");
        fs::create_dir_all(&target).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(ensure_private_dir(&junction).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
