//! In-app plugin installation via the official `dsh plugin` CLI.
//!
//! Process-tree guarantees come from reusing dsh-sidecar's `PlatformChild`
//! (unix process group / Windows Job Object): cancel and app-exit always
//! clean the whole node → dsh → pnpm → node-gyp tree. Upstream's init /
//! reconcile logic is untouched — we only make sure it finds a pnpm.

use dsh_sidecar::platform::PlatformChild;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
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

/// The runner state managed by the shell: single-flight flag + the live
/// child handle (for cancel and for app-exit cleanup).
pub struct PluginRunner {
    pub busy: AtomicBool,
    pub child: Mutex<Option<PlatformChild>>,
}

impl PluginRunner {
    pub fn new() -> Self {
        PluginRunner {
            busy: AtomicBool::new(false),
            child: Mutex::new(None),
        }
    }

    /// App-exit cleanup (C1 process-tree guarantee): a running `dsh plugin`
    /// tree is a separate process group / Job Object from the sidecar's
    /// Harness tree, so it must be killed explicitly — on unix it would
    /// otherwise be orphaned once the shell exits. Taking the handle also
    /// keeps the done-path from racing the kill. Polite signal first, then
    /// hard kill: the app is going away, there is no grace period to wait.
    pub fn shutdown(&self) {
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
}
