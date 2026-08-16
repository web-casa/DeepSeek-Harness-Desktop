//! Shared library surface of dsh-sidecar.
//!
//! The Tauri shell reuses this crate for its own supervised spawns (plugin
//! installs) so every child process gets the SAME tree guarantees as the
//! Harness: unix process group + Windows Job Object, injection-safe env
//! sanitization, and the same argument quoting rules.

pub mod platform;

/// Quote one argument for the Windows command line (CommandLineToArgvW
/// semantics). Pure string logic — unit-tested on every platform even though
/// only the Windows spawn path consumes it.
pub fn quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    // Newlines/CRs also force quoting: unquoted they act as whitespace for
    // CommandLineToArgvW and would split the argument; quoted they are
    // literal (they cannot legally appear in an argv, but must not corrupt
    // the parse of the surrounding command line either way).
    if !arg.contains([' ', '\t', '"', '\n', '\r']) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Child environment hygiene: the sidecar inherits the parent's (Tauri shell's)
// environment and forwards it to the bundled Node. A user launching the app
// from a shell with NODE_OPTIONS / NODE_PATH / npm_config_* set would otherwise
// inject code (`--require=…`) or config into the Harness process. These keys
// are stripped before spawn. The `env` overrides carried by the start command
// are applied AFTER the filter and are NOT filtered — they are the app's own
// contract (DSH_HOME etc.). Also scrubbed: ELECTRON_RUN_AS_NODE, which would
// turn a bundled Electron's node into a Harness host if one is ever reused,
// and the dynamic-linker injection primitives (DYLD_*/LD_*).
// ---------------------------------------------------------------------------

const FORBIDDEN_ENV_KEYS: [&str; 7] = [
    "node_options",
    "node_path",
    "electron_run_as_node",
    "dyld_insert_libraries",
    "dyld_library_path",
    "ld_preload",
    "ld_library_path",
];
const FORBIDDEN_ENV_PREFIX: &str = "npm_config_";

fn env_key_forbidden(key: &str) -> bool {
    let folded = key.to_ascii_lowercase();
    FORBIDDEN_ENV_KEYS.contains(&folded.as_str()) || folded.starts_with(FORBIDDEN_ENV_PREFIX)
}

/// Filter an inherited environment snapshot (the unix path).
///
/// OsString end-to-end: `std::env::vars()` PANICS on any non-UTF-8 key/value
/// (a library-level panic clippy cannot see, and production code must not
/// panic), while `Command`'s native inheritance passes raw bytes through.
/// Values therefore stay OsString (non-UTF-8 values forwarded verbatim); only
/// the KEY is UTF-8-checked, and a non-UTF-8 key simply cannot match any
/// ASCII forbidden name.
pub fn sanitize_inherited_env(
    vars: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vars.into_iter()
        .filter(|(k, _)| !k.to_str().is_some_and(env_key_forbidden))
        .collect()
}

/// ASCII case fold for one UTF-16 code unit (non-ASCII units unchanged).
/// Windows env-key lookups are nominally case-insensitive per NLS, but every
/// key this project filters is ASCII, so ASCII folding is exact here and
/// leaves non-ASCII units (unpaired surrogates included) untouched.
pub fn fold_ascii_u16(w: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&w) {
        w + (b'a' - b'A') as u16
    } else {
        w
    }
}

/// Filter raw UTF-16 env-block lines (the Windows path) WITHOUT any UTF-8
/// round-trip: untouched entries — including ones containing unpaired
/// surrogates — are forwarded verbatim. Entries without '=' and entries with
/// an empty key (the `=C:=…` per-drive entries) are never filter targets.
pub fn sanitize_env_lines(lines: Vec<Vec<u16>>) -> Vec<Vec<u16>> {
    let prefix: Vec<u16> = FORBIDDEN_ENV_PREFIX.encode_utf16().collect();
    lines
        .into_iter()
        .filter(|line| {
            let Some(eq) = line.iter().position(|&w| w == b'=' as u16) else {
                return true; // malformed entry without '=': forward untouched
            };
            if eq == 0 {
                return true; // hidden/drive entry (`=C:=…`): keep
            }
            let folded: Vec<u16> = line[..eq].iter().map(|&w| fold_ascii_u16(w)).collect();
            !FORBIDDEN_ENV_KEYS
                .iter()
                .any(|k| folded == k.encode_utf16().collect::<Vec<u16>>())
                && !folded.starts_with(&prefix)
        })
        .collect()
}
