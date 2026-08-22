//! Best-effort redaction shared by live diagnostics and opt-in detailed logs.
//!
//! This is intentionally small and dependency-free. It is not a guarantee
//! that arbitrary process output contains no sensitive data: the controller
//! always tells people to inspect a diagnostic archive before sharing it.

/// Mask a small set of high-risk token shapes and local paths from evidence.
///
/// `dsh_home` is supplied by the trusted runtime state rather than by a UI
/// caller. The current account's conventional home paths are also removed so
/// an opt-in error trace does not normally expose the account name merely
/// because the error occurred outside DSH_HOME.
pub fn redact(text: &str, dsh_home: &str) -> String {
    let mut output = text.to_owned();
    for (path, replacement) in private_path_replacements(dsh_home) {
        output = output.replace(&path, replacement);
    }

    let mut result = String::with_capacity(output.len());
    let mut rest = output.as_str();
    while !rest.is_empty() {
        let bytes = rest.as_bytes();
        if bytes.starts_with(b"sk-") {
            let token = bytes
                .iter()
                .skip(3)
                // OpenAI-style project keys include a second hyphen
                // (for example `sk-proj-…`).  Treat punctuation that is
                // normal inside opaque API-token identifiers as part of the
                // token rather than leaking the tail after the first `-`.
                .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                .count();
            if token >= 16 {
                result.push_str("sk-***");
                rest = &rest[3 + token..];
                continue;
            }
        }
        if starts_with_ascii_case_insensitive(bytes, b"bearer ") {
            let token = bytes
                .iter()
                .skip(7)
                .take_while(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                })
                .count();
            if token >= 8 {
                result.push_str("Bearer ***");
                rest = &rest[7 + token..];
                continue;
            }
        }
        if bytes.starts_with(b"AKIA") {
            let token = bytes
                .iter()
                .skip(4)
                .take_while(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                .count();
            if token >= 12 {
                result.push_str("AKIA***");
                rest = &rest[4 + token..];
                continue;
            }
        }
        match rest.chars().next() {
            Some(character) => {
                result.push(character);
                rest = &rest[character.len_utf8()..];
            }
            None => break,
        }
    }
    result
}

fn private_path_replacements(dsh_home: &str) -> Vec<(String, &'static str)> {
    let mut values = Vec::new();
    if !dsh_home.is_empty() {
        push_path_variants(&mut values, dsh_home, "<DSH_HOME>");
    }
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key) {
            let value = value.to_string_lossy().into_owned();
            // Do not replace a broad root such as `/`: it would both make an
            // archive unreadable and create a misleading privacy guarantee.
            if value.len() >= 4 {
                push_path_variants(&mut values, &value, "<USER_HOME>");
            }
        }
    }
    let windows_home = std::env::var_os("HOMEDRIVE")
        .zip(std::env::var_os("HOMEPATH"))
        .map(|(drive, path)| format!("{}{}", drive.to_string_lossy(), path.to_string_lossy()));
    if let Some(value) = windows_home.filter(|value| value.len() >= 4) {
        push_path_variants(&mut values, &value, "<USER_HOME>");
    }
    // Redact the most specific path first, and do not let an equivalent home
    // value overwrite the stronger DSH_HOME label.
    values.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    values.dedup_by(|(left, _), (right, _)| left == right);
    values
}

/// Node errors can render a Windows filesystem path as a `file:///C:/…` URL
/// or a forward-slash path even when Rust received the same path with `\\`.
/// Preserve both exact spellings so opt-in diagnostics do not expose a user
/// directory solely because the emitting component chose a different path
/// notation. This intentionally does not try to infer a broader parent path.
fn push_path_variants(
    values: &mut Vec<(String, &'static str)>,
    path: &str,
    replacement: &'static str,
) {
    values.push((path.to_string(), replacement));
    let slash_path = if path.contains('\\') {
        path.replace('\\', "/")
    } else {
        path.to_string()
    };
    if slash_path != path {
        values.push((slash_path.clone(), replacement));
    }
    // Node often renders filesystem paths inside `file:///` URLs, percent
    // encoding spaces and non-ASCII UTF-8 bytes along the way. Keeping this
    // narrowly scoped to the known private path prevents a detailed trace
    // from leaking a user directory merely because Node selected URL syntax.
    let encoded = percent_encode_path(&slash_path);
    if encoded != slash_path {
        values.push((encoded, replacement));
    }
}

fn starts_with_ascii_case_insensitive(value: &[u8], prefix: &[u8]) -> bool {
    value.len() >= prefix.len()
        && value
            .iter()
            .zip(prefix)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn percent_encode_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn masks_known_tokens_without_breaking_utf8() {
        let input = "日志 /private/dsh sk-abcdefghijklmnop sk-proj-abcdefghijklmnop Bearer abcdefgh AKIAABCDEFGHIJKLMN";
        assert_eq!(
            redact(input, "/private/dsh"),
            "日志 <DSH_HOME> sk-*** sk-*** Bearer *** AKIA***"
        );
        assert_eq!(redact("sk-abcdefghijklmno", ""), "sk-abcdefghijklmno");
    }

    #[test]
    fn dsh_home_wins_over_a_parent_home_path() {
        let replacements = private_path_replacements("/home/example/.dsh");
        assert_eq!(
            replacements
                .iter()
                .find(|(path, _)| path == "/home/example/.dsh")
                .map(|(_, label)| *label),
            Some("<DSH_HOME>")
        );
    }

    #[test]
    fn redacts_the_forward_slash_variant_of_a_windows_path() {
        assert_eq!(
            redact(
                "failed at file:///C:/Users/Ada/.dsh/profiles/web",
                r"C:\Users\Ada\.dsh",
            ),
            "failed at file:///<DSH_HOME>/profiles/web"
        );
    }

    #[test]
    fn redacts_percent_encoded_windows_paths_and_case_insensitive_bearer_tokens() {
        assert_eq!(
            redact(
                "failed at file:///C:/Users/Ada%20Lovelace/.dsh and bearer abcdefghijkl",
                r"C:\Users\Ada Lovelace\.dsh",
            ),
            "failed at file:///<DSH_HOME> and Bearer ***"
        );
    }
}
