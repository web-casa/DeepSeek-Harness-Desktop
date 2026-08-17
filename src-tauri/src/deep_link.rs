//! Strict parser and dispatcher for `dsharness://plugin/install` deep links.
//!
//! The market page (cordis.run) is the only v1 producer. A deep link never
//! installs anything by itself: parsing yields a validated request that is
//! surfaced in the bootstrap UI, and installation only starts after the
//! user clicks the confirmation button there.
//!
//! Trust boundary: the raw URL comes from the OS / browser, so it is treated
//! as attacker-controlled input. Everything is re-validated here in Rust —
//! scheme, host, path, protocol version, npm package name, and the
//! `source` page URL (https + `cordis.run` plugin-page path only). Unknown
//! query parameters are ignored so v1 links stay forward-compatible, but
//! duplicate known parameters are rejected (they are ambiguous).

use serde::Serialize;
use std::collections::HashSet;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_deep_link::DeepLinkExt;
use url::Url;

pub const SCHEME: &str = "dsharness";
pub const HOST: &str = "plugin";
pub const PATH: &str = "/install";
pub const SOURCE_HOST: &str = "cordis.run";
pub const EVENT: &str = "plugin-install-request";
const MAX_URL_LEN: usize = 2048;

/// Validated install request shown in the bootstrap confirmation dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginInstallRequest {
    pub name: String,
    pub source: String,
}

/// Single pending deep-link request. It is stored because a cold start may
/// deliver the URL before the webview has subscribed to `EVENT`; the UI
/// drains it with `get_pending_plugin_install` on mount and dismisses it
/// when the user cancels.
#[derive(Default)]
pub struct PendingPluginInstall {
    inner: Mutex<Option<PluginInstallRequest>>,
}

impl PendingPluginInstall {
    /// Store the FIRST request until the UI takes it (or dismisses it).
    /// A second valid link while the confirmation dialog is open must not
    /// overwrite the slot: the UI already ignores different requests, and
    /// this keeps the Rust slot consistent with that first-request-wins
    /// semantics instead of silently remembering a request the UI refused.
    pub fn replace(&self, request: PluginInstallRequest) {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(request);
        }
    }

    pub fn take(&self) -> Option<PluginInstallRequest> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub fn clear(&self) {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

fn query_pairs(url: &Url) -> Vec<(String, String)> {
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

/// Validate the `source` parameter: an https plugin page on cordis.run with
/// no port, credentials, query string, fragment, or trailing slash after the
/// slug. It is display-only, but keeping it canonical avoids UI ambiguity
/// and future abuse as an open-redirect-ish vector.
fn validate_source(raw: &str) -> Result<String, String> {
    let source = Url::parse(raw).map_err(|e| format!("invalid source URL: {e}"))?;
    if source.scheme() != "https" {
        return Err("source must use https".to_string());
    }
    if source.host_str() != Some(SOURCE_HOST) {
        return Err("source host must be cordis.run".to_string());
    }
    if !source.username().is_empty() || source.password().is_some() {
        return Err("source must not contain credentials".to_string());
    }
    if source.port().is_some() {
        return Err("source must not specify a port".to_string());
    }
    if source.query().is_some() || source.fragment().is_some() {
        return Err("source must not contain a query string or fragment".to_string());
    }

    let path = source.path();
    let slug = path
        .strip_prefix("/plugins/")
        .or_else(|| path.strip_prefix("/en/plugins/"))
        .ok_or_else(|| "source must be a cordis.run plugin page".to_string())?;
    if slug.is_empty() || slug.contains('/') {
        return Err("source must point to one plugin slug".to_string());
    }

    Ok(source.to_string())
}

/// Parse and validate a raw `dsharness://plugin/install?...` URL.
pub fn parse_install_url(raw: &str) -> Result<PluginInstallRequest, String> {
    if raw.len() > MAX_URL_LEN {
        return Err(format!("deep link exceeds {MAX_URL_LEN} bytes"));
    }

    let url = Url::parse(raw).map_err(|e| format!("invalid deep link URL: {e}"))?;
    if !url.scheme().eq_ignore_ascii_case(SCHEME) {
        return Err(format!("scheme must be {SCHEME}"));
    }
    if !url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case(HOST))
    {
        return Err(format!("host must be {HOST}"));
    }
    if url.path() != PATH {
        return Err(format!("path must be {PATH}"));
    }
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err("deep link must not contain credentials or a port".to_string());
    }
    if url.fragment().is_some() {
        return Err("deep link must not contain a fragment".to_string());
    }

    let pairs = query_pairs(&url);
    let mut known: HashSet<&str> = HashSet::new();
    let mut version = None;
    let mut name = None;
    let mut source = None;
    for (key, value) in &pairs {
        match key.as_str() {
            "v" | "name" | "source" if !known.insert(key.as_str()) => {
                return Err(format!("duplicate query parameter {key:?}"));
            }
            _ => {}
        }
        match key.as_str() {
            "v" => version = Some(value),
            "name" => name = Some(value),
            "source" => source = Some(value),
            _ => {}
        }
    }

    let version = version.ok_or_else(|| "missing query parameter 'v'".to_string())?;
    if version != "1" {
        return Err(format!("unsupported protocol version {version:?}"));
    }
    let name = name.ok_or_else(|| "missing query parameter 'name'".to_string())?;
    if !crate::plugins::is_valid_package_name(name) {
        return Err(format!("invalid npm package name {name:?}"));
    }
    let source = source.ok_or_else(|| "missing query parameter 'source'".to_string())?;
    let source = validate_source(source)?;

    Ok(PluginInstallRequest {
        name: name.clone(),
        source,
    })
}

/// Bring the bootstrap window back when a VALID deep link arrives. macOS
/// does not go through the single-instance callback (RunEvent::Opened is
/// delivered straight to the deep-link plugin), so a warm link would
/// otherwise render the confirmation dialog inside a hidden window.
///
/// Invalid links never reach this function: a hostile page must not be able
/// to steal focus with a malformed dsharness:// URL.
fn reveal_bootstrap<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("bootstrap") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// Process URLs delivered by the deep-link plugin (warm launches on macOS,
/// and Windows/Linux launches funneled through single-instance). The first
/// valid install URL wins; invalid URLs are logged and ignored — a malformed
/// market link must never surface an attacker-controlled dialog.
pub fn process_urls<R: Runtime>(app: &AppHandle<R>, urls: Vec<Url>) {
    for url in urls {
        let raw = url.to_string();
        match parse_install_url(&raw) {
            Ok(request) => {
                reveal_bootstrap(app);
                if let Some(pending) = app.try_state::<PendingPluginInstall>() {
                    pending.replace(request.clone());
                }
                let _ = app.emit(EVENT, &request);
                return;
            }
            Err(error) => {
                eprintln!("[deep-link] ignored {raw}: {error}");
            }
        }
    }
}

/// Register the app-lifetime deep-link listeners and drain any URL that was
/// delivered before the bootstrap webview could subscribe (cold start).
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    let app_for_events = app.clone();
    let _event_id = app.deep_link().on_open_url(move |event| {
        process_urls(&app_for_events, event.urls());
    });
    process_current(app);
}

fn process_current<R: Runtime>(app: &AppHandle<R>) {
    match app.deep_link().get_current() {
        Ok(Some(urls)) => process_urls(app, urls),
        Ok(None) => {}
        Err(error) => eprintln!("[deep-link] cannot read current URL: {error}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> PluginInstallRequest {
        parse_install_url(raw).expect("deep link should parse")
    }

    #[test]
    fn parses_scoped_and_plain_packages() {
        let scoped = parse(
            "dsharness://plugin/install?v=1&name=%40cordisjs%2Fplugin-example&\
             source=https%3A%2F%2Fcordis.run%2Fplugins%2Fexample",
        );
        assert_eq!(scoped.name, "@cordisjs/plugin-example");
        assert_eq!(scoped.source, "https://cordis.run/plugins/example");

        let plain = parse(
            "dsharness://plugin/install?v=1&name=is-odd&\
             source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
        );
        assert_eq!(plain.name, "is-odd");
        assert_eq!(plain.source, "https://cordis.run/plugins/is-odd");
    }

    #[test]
    fn accepts_english_market_pages_and_normalizes_case() {
        let request = parse(
            "dsharness://PLUGIN/install?v=1&name=is-odd&\
             source=https%3A%2F%2FCORDIS.RUN%2Fen%2Fplugins%2Fis-odd",
        );
        assert_eq!(request.source, "https://cordis.run/en/plugins/is-odd");
    }

    #[test]
    fn pending_slot_keeps_first_request_until_taken() {
        let pending = PendingPluginInstall::default();
        let first = PluginInstallRequest {
            name: "is-odd".to_string(),
            source: "https://cordis.run/plugins/is-odd".to_string(),
        };
        let second = PluginInstallRequest {
            name: "is-even".to_string(),
            source: "https://cordis.run/plugins/is-even".to_string(),
        };
        pending.replace(first.clone());
        pending.replace(second.clone());
        assert_eq!(pending.take(), Some(first));
        assert!(pending.take().is_none());
        pending.replace(second.clone());
        assert_eq!(pending.take(), Some(second));
    }

    #[test]
    fn accepts_unencoded_at_in_query_value() {
        // `?name=@scope/pkg` is valid URL syntax: `@` and `/` inside the
        // query component do not change the URL structure. The parser must
        // accept it exactly like the percent-encoded market link.
        let request = parse(
            "dsharness://plugin/install?v=1&name=@scope/pkg&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fscope-pkg",
        );
        assert_eq!(request.name, "@scope/pkg");
    }

    #[test]
    fn ignores_unknown_parameters_for_forward_compatibility() {
        let request = parse(
            "dsharness://plugin/install?v=1&name=is-odd&\
             source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd&future=1",
        );
        assert_eq!(request.name, "is-odd");
    }

    #[test]
    fn rejects_wrong_scheme_host_path_and_version() {
        for raw in [
            "https://plugin/install?v=1&name=is-odd&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
            "dsharness://other/install?v=1&name=is-odd&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
            "dsharness://plugin/uninstall?v=1&name=is-odd&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
            "dsharness://plugin/install?v=2&name=is-odd&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
            "dsharness://plugin/install?name=is-odd&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
        ] {
            assert!(parse_install_url(raw).is_err(), "should reject {raw}");
        }
    }

    #[test]
    fn rejects_duplicate_known_query_parameters() {
        for raw in [
            "dsharness://plugin/install?v=1&v=1&name=is-odd&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
            "dsharness://plugin/install?v=1&name=is-odd&name=is-even&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
            "dsharness://plugin/install?v=1&name=is-odd&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fother",
        ] {
            assert!(parse_install_url(raw).is_err(), "should reject {raw}");
        }
    }

    #[test]
    fn rejects_invalid_package_names() {
        for name in [
            "Is-Odd",
            "pkg space",
            "foo@1.2.3",
            "-D",
            "@scope",
            "@scope//x",
            "@scope/pkg@1",
            "pkg!",
            "pkg`whoami`",
            "",
        ] {
            let raw = format!(
                "dsharness://plugin/install?v=1&name={}&\
                 source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd",
                urlencoding(name)
            );
            assert!(
                parse_install_url(&raw).is_err(),
                "should reject package name {name:?}"
            );
        }
    }

    #[test]
    fn rejects_non_canonical_sources() {
        for source in [
            "http://cordis.run/plugins/is-odd",
            "https://evil.example/plugins/is-odd",
            "https://cordis.run.evil.example/plugins/is-odd",
            "https://cordis.run:8443/plugins/is-odd",
            "https://cordis.run/plugins/is-odd?utm_source=market",
            "https://cordis.run/plugins/is-odd#install",
            "https://cordis.run/",
            "https://cordis.run/plugins/",
            "https://cordis.run/plugins/is-odd/",
            "https://user:pass@cordis.run/plugins/is-odd",
        ] {
            let raw = format!(
                "dsharness://plugin/install?v=1&name=is-odd&source={}",
                urlencoding(source)
            );
            assert!(
                parse_install_url(&raw).is_err(),
                "should reject source {source:?}"
            );
        }
    }

    #[test]
    fn rejects_oversized_links_and_fragments() {
        let long = format!(
            "dsharness://plugin/install?v=1&name={}&source=https%3A%2F%2Fcordis.run%2Fplugins%2Fx",
            "a".repeat(2048)
        );
        assert!(parse_install_url(&long).is_err());
        assert!(parse_install_url(
            "dsharness://plugin/install?v=1&name=is-odd&\
                 source=https%3A%2F%2Fcordis.run%2Fplugins%2Fis-odd#frag"
        )
        .is_err());
    }

    fn urlencoding(value: &str) -> String {
        let mut out = String::new();
        for byte in value.as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                out.push(*byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }
}
