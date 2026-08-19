//! Curated plugin allowlist for Store builds.
//!
//! The Store product may only install plugins reviewed on cordis.run. The
//! snapshot is committed so a Store build never depends on a live fetch from
//! the marketplace; refresh it with the maintainer checklist when the
//! marketplace list changes.

use serde::Deserialize;

#[derive(Deserialize)]
struct CuratedPlugins {
    #[allow(dead_code)]
    version: u32,
    #[allow(dead_code)]
    source: String,
    #[allow(dead_code)]
    #[serde(rename = "reviewedAt")]
    reviewed_at: String,
    allowlist: Vec<String>,
}

const CURATED_JSON: &str = include_str!("../store-curated-plugins.json");

pub fn is_allowed(name: &str) -> bool {
    let name = name.trim();
    serde_json::from_str::<CuratedPlugins>(CURATED_JSON)
        .map(|list| list.allowlist.iter().any(|item| item == name))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::is_allowed;

    #[test]
    fn snapshot_parses_and_matches_exact_names() {
        assert!(is_allowed("dsh-cc-tui"));
        assert!(is_allowed("@oh-dsh/desktop"));
        assert!(!is_allowed("not-reviewed"));
        assert!(is_allowed(" dsh-cc-tui "));
    }
}
