//! Resolve where the bundled runtime pieces live.
//!
//! Resolution order:
//!   1. `DSH_RUNTIME_DIR` env override (dev/testing on any platform);
//!   2. debug builds (`tauri dev`): the staged repo dir `resources/runtime`;
//!   3. production: `<resource dir>/runtime` inside the app bundle.
//!
//! DSH_HOME: `DSH_HOME` env override, else the platform app-data dir under a
//! `harness/` subfolder — deliberately isolated from the CLI's `~/.dsh` so a
//! pinned Desktop runtime can never corrupt a user's CLI profiles.

use std::path::{Path, PathBuf};
use tauri::{path::BaseDirectory, Manager};

#[derive(Clone)]
pub struct RuntimePaths {
    pub sidecar: PathBuf,
    pub node: PathBuf,
    pub harness_dir: PathBuf,
    pub dsh_home: PathBuf,
}

/// Keep a developer override from targeting a filesystem root. The harness
/// initializer protects its data root with mode 0700 on Unix, so accepting
/// `/`, a drive root, or a UNC share root would make an accidental privileged
/// launch capable of changing a system directory's permissions.
fn validate_dsh_home_override(home: &Path) -> Result<(), String> {
    if home.as_os_str().is_empty() || !home.is_absolute() {
        return Err("DSH_HOME must be a non-empty absolute path".to_string());
    }
    if home.parent().is_none() {
        return Err("DSH_HOME must not be a filesystem root".to_string());
    }
    Ok(())
}

pub fn resolve(app: &tauri::AppHandle) -> Result<RuntimePaths, String> {
    let exe_suffix = if cfg!(windows) { ".exe" } else { "" };
    let sidecar_name = format!("sidecar{exe_suffix}");
    let node_name = format!("node{exe_suffix}");

    let (sidecar, node, harness_dir) = if let Ok(d) = std::env::var("DSH_RUNTIME_DIR") {
        let base = PathBuf::from(d);
        if base.as_os_str().is_empty() {
            return Err("DSH_RUNTIME_DIR is set but empty".to_string());
        }
        if !base.is_absolute() {
            return Err("DSH_RUNTIME_DIR must be an absolute path".to_string());
        }
        (
            base.join(&sidecar_name),
            base.join(&node_name),
            base.join("harness"),
        )
    } else if cfg!(debug_assertions) {
        // `tauri dev`: the staged repo resources dir (src-tauri/resources/runtime).
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/runtime");
        (
            base.join(&sidecar_name),
            base.join(&node_name),
            base.join("harness"),
        )
    } else {
        let base = app
            .path()
            .resolve("runtime", BaseDirectory::Resource)
            .map_err(|e| format!("无法解析运行时资源目录: {e}"))?;
        (
            base.join(&sidecar_name),
            base.join(&node_name),
            base.join("harness"),
        )
    };

    let dsh_home = if let Ok(h) = std::env::var("DSH_HOME") {
        let home = PathBuf::from(&h);
        validate_dsh_home_override(&home)?;
        home
    } else {
        app.path()
            .app_data_dir()
            .map_err(|e| format!("无法解析应用数据目录: {e}"))?
            .join("harness")
    };

    Ok(RuntimePaths {
        sidecar,
        node,
        harness_dir,
        dsh_home,
    })
}

#[cfg(test)]
mod tests {
    use super::validate_dsh_home_override;
    use std::path::Path;

    #[test]
    fn dsh_home_override_rejects_filesystem_root() {
        // POSIX permits repeated leading separators. They still name the same
        // root and must not bypass the permission-changing guard.
        for root in [Path::new("/"), Path::new("//"), Path::new("///")] {
            assert!(
                validate_dsh_home_override(root).is_err(),
                "must reject {root:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn dsh_home_override_rejects_a_windows_drive_root() {
        assert!(validate_dsh_home_override(Path::new(r"C:\")).is_err());
    }

    #[test]
    fn dsh_home_override_keeps_the_non_empty_absolute_path_requirement() {
        for invalid in [Path::new(""), Path::new("relative/dsh-home")] {
            assert!(
                validate_dsh_home_override(invalid).is_err(),
                "must reject {invalid:?}"
            );
        }
    }

    #[test]
    fn dsh_home_override_accepts_a_real_absolute_child_directory() {
        let child = std::env::temp_dir().join("dsh-desktop-test-home");
        assert!(validate_dsh_home_override(&child).is_ok());
    }
}
