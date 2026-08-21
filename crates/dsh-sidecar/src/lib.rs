//! Shared library surface of dsh-sidecar.
//!
//! The Tauri shell reuses this crate for its own supervised spawns (plugin
//! installs) so every child process gets the SAME tree guarantees as the
//! Harness: unix process group + Windows Job Object, injection-safe env
//! sanitization, and the same argument quoting rules.

pub mod platform;

/// Stream newline-delimited child output without ever buffering an unbounded
/// line. `BufRead::lines()` allocates until it sees a newline, so truncating
/// the returned `String` is too late to protect the supervisor from a child
/// that writes one enormous line. This reader keeps at most `max_bytes`,
/// discards the rest of that line in-place, and then resumes at the next one.
///
/// The callback returns `false` to stop early (for example when its channel
/// receiver has gone away). Invalid child bytes are represented lossily; an
/// incomplete UTF-8 character at the truncation boundary is removed before
/// the marker is appended.
pub fn for_each_bounded_line<R, F>(
    mut reader: R,
    max_bytes: usize,
    mut callback: F,
) -> std::io::Result<()>
where
    R: std::io::BufRead,
    F: FnMut(String) -> bool,
{
    if max_bytes == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "bounded line size must be greater than zero",
        ));
    }

    loop {
        let mut prefix = Vec::with_capacity(max_bytes.min(8 * 1024));
        let mut truncated = false;
        let mut saw_any = false;
        loop {
            let (consumed, newline, eof) = {
                let available = reader.fill_buf()?;
                if available.is_empty() {
                    (0, false, true)
                } else {
                    saw_any = true;
                    let newline_at = available.iter().position(|byte| *byte == b'\n');
                    let content_len = newline_at.unwrap_or(available.len());
                    let remaining = max_bytes.saturating_sub(prefix.len());
                    let copy_len = content_len.min(remaining);
                    prefix.extend_from_slice(&available[..copy_len]);
                    truncated |= content_len > copy_len;
                    (
                        content_len + usize::from(newline_at.is_some()),
                        newline_at.is_some(),
                        false,
                    )
                }
            };
            if eof {
                if !saw_any {
                    return Ok(());
                }
                break;
            }
            reader.consume(consumed);
            if newline {
                break;
            }
        }

        if prefix.last() == Some(&b'\r') {
            prefix.pop();
        }
        if truncated {
            if let Err(error) = std::str::from_utf8(&prefix) {
                if error.error_len().is_none() {
                    prefix.truncate(error.valid_up_to());
                }
            }
        }
        let mut line = String::from_utf8_lossy(&prefix).into_owned();
        if truncated {
            line.push_str("… [line truncated]");
        }
        if !callback(line) {
            return Ok(());
        }
    }
}

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

/// Convert the two Win32 verbatim path forms that Node cannot use as a main
/// entrypoint back to their ordinary DOS/UNC spelling.
///
/// Rust and Win32 APIs correctly accept `\\?\C:\…`, and Tauri can return it
/// for a packaged resource directory. Node's `resolveMainPath`, however,
/// currently reduces that form to `C:` and fails with `EISDIR`. Only canonical
/// drive-absolute and UNC forms are converted; device/volume namespaces and
/// malformed inputs keep their original spelling rather than broadening the
/// path semantics of an untrusted start command.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn node_compatible_windows_path(path: &str) -> String {
    const VERBATIM_PREFIX: &str = "\\\\?\\";
    const VERBATIM_UNC_PREFIX: &str = "\\\\?\\UNC\\";

    let strip_ascii_prefix = |prefix: &str| {
        path.get(..prefix.len())
            .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
            .map(|_| &path[prefix.len()..])
    };

    if let Some(rest) = strip_ascii_prefix(VERBATIM_UNC_PREFIX) {
        let mut parts = rest.split(['\\', '/']).filter(|part| !part.is_empty());
        if parts.next().is_some() && parts.next().is_some() {
            return format!("\\\\{rest}");
        }
    }

    if let Some(rest) = strip_ascii_prefix(VERBATIM_PREFIX) {
        let bytes = rest.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/')
        {
            return rest.to_string();
        }
    }

    path.to_string()
}

// ---------------------------------------------------------------------------
// Child environment hygiene: the sidecar inherits the parent's (Tauri shell's)
// environment and forwards it to the bundled Node. A user launching the app
// from a shell with NODE_OPTIONS / NODE_PATH / npm_config_* / pnpm_config_*
// set would otherwise inject code (`--require=…`) or config into the Harness
// process. Node TLS controls are also stripped: NODE_TLS_REJECT_UNAUTHORIZED
// can silently disable verification, while NODE_EXTRA_CA_CERTS can append an
// attacker-controlled certificate authority. The `env` overrides carried by
// the start command are applied AFTER the filter and are NOT filtered —
// they are the app's own contract (DSH_HOME etc.). Also scrubbed:
// ELECTRON_RUN_AS_NODE, which would turn a bundled Electron's node into a
// Harness host if one is ever reused, and the dynamic-linker injection
// primitives (DYLD_*/LD_*).
// ---------------------------------------------------------------------------

const FORBIDDEN_ENV_KEYS: [&str; 10] = [
    "node_options",
    "node_path",
    "node_tls_reject_unauthorized",
    "node_extra_ca_certs",
    // npm/pnpm use this self-referential control value when spawning helpers;
    // Desktop always supplies its own bundled entrypoint instead.
    "npm_execpath",
    "electron_run_as_node",
    "dyld_insert_libraries",
    "dyld_library_path",
    "ld_preload",
    "ld_library_path",
];
const FORBIDDEN_ENV_PREFIXES: [&str; 2] = ["npm_config_", "pnpm_config_"];

fn env_key_forbidden(key: &str) -> bool {
    let folded = key.to_ascii_lowercase();
    FORBIDDEN_ENV_KEYS.contains(&folded.as_str())
        || FORBIDDEN_ENV_PREFIXES
            .iter()
            .any(|prefix| folded.starts_with(prefix))
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
    let prefixes: Vec<Vec<u16>> = FORBIDDEN_ENV_PREFIXES
        .iter()
        .map(|prefix| prefix.encode_utf16().collect())
        .collect();
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
                && !prefixes.iter().any(|prefix| folded.starts_with(prefix))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod bounded_line_tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn bounded_reader_discards_large_line_and_resumes() {
        let input = format!("{}\nnext\r\nlast", "x".repeat(64 * 1024));
        let mut lines = Vec::new();
        for_each_bounded_line(BufReader::new(input.as_bytes()), 32, |line| {
            lines.push(line);
            true
        })
        .unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], format!("{}… [line truncated]", "x".repeat(32)));
        assert_eq!(lines[1], "next");
        assert_eq!(lines[2], "last");
    }

    #[test]
    fn bounded_reader_keeps_utf8_boundary() {
        let input = format!("{}\nafter\n", "你".repeat(128));
        let mut lines = Vec::new();
        for_each_bounded_line(BufReader::new(input.as_bytes()), 10, |line| {
            lines.push(line);
            true
        })
        .unwrap();
        assert_eq!(lines[0], "你你你… [line truncated]");
        assert_eq!(lines[1], "after");
    }

    #[test]
    fn bounded_reader_can_stop_without_reading_the_tail() {
        let input = b"one\ntwo\nthree\n";
        let mut lines = Vec::new();
        for_each_bounded_line(BufReader::new(input.as_slice()), 16, |line| {
            lines.push(line);
            false
        })
        .unwrap();
        assert_eq!(lines, ["one"]);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod windows_node_path_tests {
    use super::node_compatible_windows_path;

    #[test]
    fn converts_only_supported_verbatim_dos_and_unc_paths() {
        assert_eq!(
            node_compatible_windows_path(r"\\?\C:\Program Files\DSH Desktop\runtime\node.exe"),
            r"C:\Program Files\DSH Desktop\runtime\node.exe"
        );
        assert_eq!(
            node_compatible_windows_path(r"\\?\unc\server\share\DSH Desktop\bin.js"),
            r"\\server\share\DSH Desktop\bin.js"
        );
    }

    #[test]
    fn leaves_noncanonical_verbatim_namespaces_unchanged() {
        for path in [
            r"C:\normal\runtime\node.exe",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\runtime\node.exe",
            r"\\?\UNC\server",
            r"\\?\C:",
        ] {
            assert_eq!(node_compatible_windows_path(path), path);
        }
    }
}
