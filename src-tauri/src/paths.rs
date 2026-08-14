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

use std::path::PathBuf;
use tauri::{Manager, path::BaseDirectory};

pub struct RuntimePaths {
    pub sidecar: PathBuf,
    pub node: PathBuf,
    pub harness_dir: PathBuf,
    pub dsh_home: PathBuf,
}

pub fn resolve(app: &tauri::AppHandle) -> RuntimePaths {
    let exe_suffix = if cfg!(windows) { ".exe" } else { "" };
    let sidecar_name = format!("sidecar{exe_suffix}");
    let node_name = format!("node{exe_suffix}");

    let (sidecar, node, harness_dir) = if let Ok(d) = std::env::var("DSH_RUNTIME_DIR") {
        let base = PathBuf::from(d);
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
            .unwrap_or_default();
        (
            base.join(&sidecar_name),
            base.join(&node_name),
            base.join("harness"),
        )
    };

    let dsh_home = if let Ok(h) = std::env::var("DSH_HOME") {
        PathBuf::from(h)
    } else {
        app.path()
            .app_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("dsh-desktop"))
            .join("harness")
    };

    RuntimePaths {
        sidecar,
        node,
        harness_dir,
        dsh_home,
    }
}
