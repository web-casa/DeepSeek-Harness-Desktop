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
pub const PRESET_HOST: &str = "preset";
pub const PRESET_PATH: &str = "/install";
pub const PRESET_EVENT: &str = "preset-install-request";
pub const MAX_REMOTE_PRESET_BYTES: u64 = 16 * 1024 * 1024;
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

/// Validated preset install request shown before any download is attempted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresetInstallRequest {
    pub url: String,
    pub source: String,
    pub slug: String,
}

/// Where a remote preset request is in its lifecycle. `ArchivePreview` is
/// intentionally NOT Serialize: it contains a local temp path that must
/// never cross IPC.
#[derive(Debug, Clone)]
pub enum RemotePresetState {
    AwaitingDownloadConsent,
    Downloading,
    AwaitingInstallConsent {
        archive: std::path::PathBuf,
        preview: crate::preset::ArchivePreview,
    },
}

/// One validated remote-preset session. `request_id` is generated from a
/// CSPRNG and is the only handle the webview may use.
#[derive(Debug, Clone)]
pub struct RemotePresetSession {
    pub request_id: String,
    pub url: String,
    pub source: String,
    pub slug: String,
    pub state: RemotePresetState,
}

/// Slot for the current remote-preset request, plus a tiny arbiter that
/// makes plugin deep links, remote-preset deep links, and local preset
/// previews mutually exclusive at the Rust layer.
#[derive(Default)]
pub struct PendingRemotePreset {
    inner: Mutex<Option<RemotePresetSession>>,
}

/// Global install-request arbiter: exactly one modal may own the bootstrap
/// UI at a time. The slot is released by the command that completes or
/// dismisses the flow.
#[derive(Default)]
pub struct InstallArbiter {
    inner: Mutex<Option<PendingInstallKind>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingInstallKind {
    Plugin,
    RemotePreset,
    LocalPresetPicker,
}

impl InstallArbiter {
    /// Acquire the global modal slot. Returns false when another flow owns it.
    pub fn try_acquire(&self, kind: PendingInstallKind) -> bool {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_some() {
            return false;
        }
        *slot = Some(kind);
        true
    }

    pub fn release(&self) {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

impl PendingRemotePreset {
    /// Enqueue only when the arbiter grants the preset modal AND the slot is
    /// empty. Returns the generated request_id on success.
    pub fn try_enqueue(
        &self,
        arbiter: &InstallArbiter,
        url: String,
        source: String,
        slug: String,
    ) -> Option<String> {
        if !arbiter.try_acquire(PendingInstallKind::RemotePreset) {
            return None;
        }
        let request_id = match new_request_id() {
            Ok(id) => id,
            Err(error) => {
                eprintln!("[deep-link] cannot generate request_id: {error}");
                arbiter.release();
                return None;
            }
        };
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_some() {
            arbiter.release();
            return None;
        }
        *slot = Some(RemotePresetSession {
            request_id: request_id.clone(),
            url,
            source,
            slug,
            state: RemotePresetState::AwaitingDownloadConsent,
        });
        Some(request_id)
    }

    /// Snapshot for cold-start drain / UI mount. Does NOT remove the slot.
    pub fn snapshot(&self) -> Option<RemotePresetSession> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Try to move AwaitingDownloadConsent -> Downloading for `request_id`.
    pub fn begin_download(&self, request_id: &str) -> Result<String, String> {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(session) = slot.as_mut() else {
            return Err("no pending remote preset request".to_string());
        };
        if session.request_id != request_id {
            return Err("request_id does not match the pending request".to_string());
        }
        if !matches!(session.state, RemotePresetState::AwaitingDownloadConsent) {
            return Err("request is not awaiting download consent".to_string());
        }
        session.state = RemotePresetState::Downloading;
        Ok(session.url.clone())
    }

    /// Try to commit a completed download for the same request. The caller
    /// must pass the path and preview; this re-checks the slot is still
    /// Downloading and owned by `request_id`.
    pub fn complete_download(
        &self,
        request_id: &str,
        archive: std::path::PathBuf,
        preview: crate::preset::ArchivePreview,
    ) -> Result<(), String> {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(session) = slot.as_mut() else {
            return Err("no pending remote preset request".to_string());
        };
        if session.request_id != request_id {
            return Err("request_id does not match the pending request".to_string());
        }
        if !matches!(session.state, RemotePresetState::Downloading) {
            return Err("request is not downloading".to_string());
        }
        if preview.id != session.slug {
            return Err(format!(
                "preset archive id {:?} does not match requested slug {:?}",
                preview.id, session.slug
            ));
        }
        session.state = RemotePresetState::AwaitingInstallConsent { archive, preview };
        Ok(())
    }

    /// Take the archive for install, leaving the slot empty but NOT releasing
    /// the arbiter (the caller releases it after success/failure cleanup).
    pub fn take_archive(
        &self,
        request_id: &str,
    ) -> Result<(std::path::PathBuf, crate::preset::ArchivePreview), String> {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(session) = slot.take() else {
            return Err("no pending remote preset request".to_string());
        };
        if session.request_id != request_id {
            *slot = Some(session);
            return Err("request_id does not match the pending request".to_string());
        }
        match session.state {
            RemotePresetState::AwaitingInstallConsent { archive, preview } => {
                Ok((archive, preview))
            }
            _ => {
                *slot = Some(session);
                Err("request is not awaiting install consent".to_string())
            }
        }
    }

    /// Remove the slot ONLY when it still belongs to `request_id`.
    /// Returns the archive path if the session had one, or None when it was
    /// removed while still in a pre-download state. Mismatches are an error
    /// and do not touch the slot.
    pub fn dismiss(&self, request_id: &str) -> Result<Option<std::path::PathBuf>, String> {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(session) = slot.as_ref() else {
            return Err("no pending remote preset request".to_string());
        };
        if session.request_id != request_id {
            return Err("request_id does not match the pending request".to_string());
        }
        if matches!(session.state, RemotePresetState::Downloading) {
            return Err("download is in progress and cannot be dismissed".to_string());
        }
        let session = match slot.take() {
            Some(session) => session,
            None => return Err("no pending remote preset request".to_string()),
        };
        Ok(match session.state {
            RemotePresetState::AwaitingInstallConsent { archive, .. } => Some(archive),
            _ => None,
        })
    }

    /// Clear the slot after a download failure ONLY when it still belongs to
    /// the same request and is in `Downloading`. Returns true when it cleared.
    pub fn fail_download(&self, request_id: &str) -> bool {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(session) = slot.as_ref() else {
            return false;
        };
        if session.request_id != request_id
            || !matches!(session.state, RemotePresetState::Downloading)
        {
            return false;
        }
        *slot = None;
        true
    }
}

fn new_request_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("CSPRNG failed: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
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

/// Validate the `url` parameter for a preset install: the cordis.run
/// direct-download endpoint. No query, no fragment, no credentials, no port.
/// v1 supports only the latest approved download path.
pub fn validate_preset_download_url(raw: &str) -> Result<String, String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid preset download URL: {e}"))?;
    if url.scheme() != "https" {
        return Err("preset download URL must use https".to_string());
    }
    if url.host_str() != Some(SOURCE_HOST) {
        return Err("preset download host must be cordis.run".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("preset download URL must not contain credentials".to_string());
    }
    if url.port().is_some() {
        return Err("preset download URL must not specify a port".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("preset download URL must not contain a query string or fragment".to_string());
    }
    let path = url.path();
    let slug = path
        .strip_prefix("/api/presets/")
        .and_then(|rest| rest.strip_suffix("/download"))
        .ok_or_else(|| {
            "preset download URL must be https://cordis.run/api/presets/<slug>/download".to_string()
        })?;
    if slug.is_empty() || slug.contains('/') {
        return Err("preset download URL must contain exactly one preset slug".to_string());
    }
    Ok(url.to_string())
}

fn preset_slug_of_download(url: &Url) -> Option<String> {
    url.path()
        .strip_prefix("/api/presets/")?
        .strip_suffix("/download")
        .map(str::to_string)
}

fn preset_slug_of_source(url: &Url) -> Option<String> {
    let path = url.path();
    path.strip_prefix("/presets/")
        .or_else(|| path.strip_prefix("/en/presets/"))
        .map(str::to_string)
}

/// Validate the `source` parameter for a preset install: a cordis.run
/// preset page. The same strict shape as plugin sources, but for presets.
pub fn validate_preset_source(raw: &str) -> Result<String, String> {
    let source = Url::parse(raw).map_err(|e| format!("invalid preset source URL: {e}"))?;
    if source.scheme() != "https" {
        return Err("preset source must use https".to_string());
    }
    if source.host_str() != Some(SOURCE_HOST) {
        return Err("preset source host must be cordis.run".to_string());
    }
    if !source.username().is_empty() || source.password().is_some() {
        return Err("preset source must not contain credentials".to_string());
    }
    if source.port().is_some() {
        return Err("preset source must not specify a port".to_string());
    }
    if source.query().is_some() || source.fragment().is_some() {
        return Err("preset source must not contain a query string or fragment".to_string());
    }
    let path = source.path();
    let slug = path
        .strip_prefix("/presets/")
        .or_else(|| path.strip_prefix("/en/presets/"))
        .ok_or_else(|| "preset source must be a cordis.run preset page".to_string())?;
    if slug.is_empty() || slug.contains('/') {
        return Err("preset source must point to one preset slug".to_string());
    }
    Ok(source.to_string())
}

/// Parse and validate a raw `dsharness://preset/install?...` URL.
pub fn parse_preset_install_url(raw: &str) -> Result<PresetInstallRequest, String> {
    if raw.len() > MAX_URL_LEN {
        return Err(format!("deep link exceeds {MAX_URL_LEN} bytes"));
    }

    let url = Url::parse(raw).map_err(|e| format!("invalid deep link URL: {e}"))?;
    if !url.scheme().eq_ignore_ascii_case(SCHEME) {
        return Err(format!("scheme must be {SCHEME}"));
    }
    if !url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case(PRESET_HOST))
    {
        return Err(format!("host must be {PRESET_HOST}"));
    }
    if url.path() != PRESET_PATH {
        return Err(format!("path must be {PRESET_PATH}"));
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
    let mut dl_url = None;
    let mut source = None;
    for (key, value) in &pairs {
        match key.as_str() {
            "v" | "url" | "source" if !known.insert(key.as_str()) => {
                return Err(format!("duplicate query parameter {key:?}"));
            }
            _ => {}
        }
        match key.as_str() {
            "v" => version = Some(value),
            "url" => dl_url = Some(value),
            "source" => source = Some(value),
            _ => {}
        }
    }

    let version = version.ok_or_else(|| "missing query parameter 'v'".to_string())?;
    if version != "1" {
        return Err(format!("unsupported protocol version {version:?}"));
    }
    let dl_url = dl_url.ok_or_else(|| "missing query parameter 'url'".to_string())?;
    let dl_url = validate_preset_download_url(dl_url)?;
    let source = source.ok_or_else(|| "missing query parameter 'source'".to_string())?;
    let source = validate_preset_source(source)?;

    let download = Url::parse(&dl_url).map_err(|e| format!("invalid download URL: {e}"))?;
    let source = Url::parse(&source).map_err(|e| format!("invalid source URL: {e}"))?;
    let slug = preset_slug_of_download(&download)
        .filter(|s| Some(s.as_str()) == preset_slug_of_source(&source).as_deref())
        .ok_or_else(|| "download URL and source must point to the same preset slug".to_string())?;

    Ok(PresetInstallRequest {
        url: download.to_string(),
        source: source.to_string(),
        slug,
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

fn handle_plugin_url<R: Runtime>(app: &AppHandle<R>, raw: &str) {
    match parse_install_url(raw) {
        Ok(request) => {
            let Some(pending) = app.try_state::<PendingPluginInstall>() else {
                eprintln!("[deep-link] plugin slot is not managed");
                return;
            };
            // The arbiter is released by get/dismiss command paths; it is
            // acquired only once per modal. If another flow owns the modal,
            // this link is rejected without stealing focus.
            let Some(arbiter) = app.try_state::<InstallArbiter>() else {
                eprintln!("[deep-link] install arbiter is not managed");
                return;
            };
            if !arbiter.try_acquire(PendingInstallKind::Plugin) {
                eprintln!("[deep-link] ignored {raw}: another install flow is active");
                return;
            }
            reveal_bootstrap(app);
            pending.replace(request.clone());
            let _ = app.emit(EVENT, &request);
        }
        Err(error) => eprintln!("[deep-link] ignored {raw}: {error}"),
    }
}

fn handle_preset_url<R: Runtime>(app: &AppHandle<R>, raw: &str) {
    match parse_preset_install_url(raw) {
        Ok(request) => {
            let Some(pending) = app.try_state::<PendingRemotePreset>() else {
                eprintln!("[deep-link] preset slot is not managed");
                return;
            };
            let Some(arbiter) = app.try_state::<InstallArbiter>() else {
                eprintln!("[deep-link] install arbiter is not managed");
                return;
            };
            let Some(request_id) =
                pending.try_enqueue(&arbiter, request.url, request.source.clone(), request.slug)
            else {
                eprintln!("[deep-link] ignored {raw}: another install flow is active");
                return;
            };
            reveal_bootstrap(app);
            let _ = app.emit(
                PRESET_EVENT,
                serde_json::json!({
                    "requestId": request_id,
                    "source": request.source,
                    "stage": "awaiting-download",
                }),
            );
        }
        Err(error) => eprintln!("[deep-link] ignored {raw}: {error}"),
    }
}

/// Process URLs delivered by the deep-link plugin (warm launches on macOS,
/// and Windows/Linux launches funneled through single-instance). The first
/// valid install URL wins; invalid URLs are logged and ignored — a malformed
/// market link must never surface an attacker-controlled dialog.
pub fn process_urls<R: Runtime>(app: &AppHandle<R>, urls: Vec<Url>) {
    for url in urls {
        let raw = url.to_string();
        if url
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(PRESET_HOST))
            && url.path() == PRESET_PATH
        {
            handle_preset_url(app, &raw);
        } else {
            handle_plugin_url(app, &raw);
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

    // ---- preset deep-link (dsharness://preset/install) ----

    fn parse_preset(raw: &str) -> PresetInstallRequest {
        parse_preset_install_url(raw).expect("preset deep link should parse")
    }

    fn preset_raw(download: &str, source: &str) -> String {
        format!(
            "dsharness://preset/install?v=1&url={}&source={}",
            urlencoding(download),
            urlencoding(source)
        )
    }

    #[test]
    fn preset_parses_canonical_download_and_source() {
        let request = parse_preset(&preset_raw(
            "https://cordis.run/api/presets/code/download",
            "https://cordis.run/presets/code",
        ));
        assert_eq!(request.url, "https://cordis.run/api/presets/code/download");
        assert_eq!(request.source, "https://cordis.run/presets/code");

        let en = parse_preset(&preset_raw(
            "https://cordis.run/api/presets/code/download",
            "https://cordis.run/en/presets/code",
        ));
        assert_eq!(en.source, "https://cordis.run/en/presets/code");
    }

    #[test]
    fn preset_rejects_wrong_scheme_host_path_and_version() {
        for raw in [
            "https://preset/install?v=1&url=https%3A%2F%2Fcordis.run%2Fapi%2Fpresets%2Fcode%2Fdownload&source=https%3A%2F%2Fcordis.run%2Fpresets%2Fcode",
            "dsharness://plugin/install?v=1&url=https%3A%2F%2Fcordis.run%2Fapi%2Fpresets%2Fcode%2Fdownload&source=https%3A%2F%2Fcordis.run%2Fpresets%2Fcode",
            "dsharness://preset/uninstall?v=1&url=https%3A%2F%2Fcordis.run%2Fapi%2Fpresets%2Fcode%2Fdownload&source=https%3A%2F%2Fcordis.run%2Fpresets%2Fcode",
            "dsharness://preset/install?v=2&url=https%3A%2F%2Fcordis.run%2Fapi%2Fpresets%2Fcode%2Fdownload&source=https%3A%2F%2Fcordis.run%2Fpresets%2Fcode",
            "dsharness://preset/install?url=https%3A%2F%2Fcordis.run%2Fapi%2Fpresets%2Fcode%2Fdownload&source=https%3A%2F%2Fcordis.run%2Fpresets%2Fcode",
        ] {
            assert!(
                parse_preset_install_url(raw).is_err(),
                "should reject {raw}"
            );
        }
    }

    #[test]
    fn preset_rejects_download_query_fragment_and_slug_mismatch() {
        for raw in [
            preset_raw(
                "https://cordis.run/api/presets/code/download?v=v1-official-code",
                "https://cordis.run/presets/code",
            ),
            preset_raw(
                "https://cordis.run/api/presets/code/download#frag",
                "https://cordis.run/presets/code",
            ),
            preset_raw(
                "https://cordis.run/api/presets/code/download",
                "https://cordis.run/presets/other",
            ),
        ] {
            assert!(
                parse_preset_install_url(&raw).is_err(),
                "should reject {raw}"
            );
        }
    }

    #[test]
    fn preset_rejects_duplicate_known_parameters() {
        for raw in [
            format!(
                "dsharness://preset/install?v=1&v=1&url={}&source={}",
                urlencoding("https://cordis.run/api/presets/code/download"),
                urlencoding("https://cordis.run/presets/code"),
            ),
            format!(
                "dsharness://preset/install?v=1&url={}&url={}&source={}",
                urlencoding("https://cordis.run/api/presets/code/download"),
                urlencoding("https://cordis.run/api/presets/other/download"),
                urlencoding("https://cordis.run/presets/code"),
            ),
        ] {
            assert!(
                parse_preset_install_url(&raw).is_err(),
                "should reject {raw}"
            );
        }
    }

    #[test]
    fn pending_remote_preset_enqueues_first_only() {
        let pending = PendingRemotePreset::default();
        let arbiter = InstallArbiter::default();
        let id1 = pending
            .try_enqueue(
                &arbiter,
                "https://cordis.run/api/presets/code/download".to_string(),
                "https://cordis.run/presets/code".to_string(),
                "code".to_string(),
            )
            .expect("first request should enqueue");
        let id2 = pending.try_enqueue(
            &arbiter,
            "https://cordis.run/api/presets/other/download".to_string(),
            "https://cordis.run/presets/other".to_string(),
            "other".to_string(),
        );
        assert!(id2.is_none(), "second request must not enqueue");
        assert!(pending.snapshot().is_some_and(|s| s.request_id == id1));
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
