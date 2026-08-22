//! Desktop-owned presentation preferences shared by the local controller,
//! tray, native menu, and window chrome.
//!
//! The remote Harness WebView never receives this state or an IPC capability.
//! Only the trusted bootstrap window may update the small, strictly validated
//! preference through the two commands defined at the end of this module.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

const PREFERENCE_SCHEMA: u8 = 1;
const PREFERENCE_MAX_BYTES: usize = 128;
const MAX_SYSTEM_LANGUAGE_TAGS: usize = 16;
const MAX_SYSTEM_LANGUAGE_TAG_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LocalePreference {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en")]
    English,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PresentationLocale {
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en")]
    English,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationSnapshot {
    pub preference: LocalePreference,
    pub locale: PresentationLocale,
    /// Whether the current preference survived the last persistence attempt.
    /// A false value is a session-only setting, never an indication that the
    /// native title/tray update was skipped.
    pub persisted: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPreference {
    schema: u8,
    preference: LocalePreference,
}

#[derive(Debug)]
struct PresentationInner {
    preference: LocalePreference,
    locale: PresentationLocale,
    persisted: bool,
}

/// Application-managed state. The path is deliberately optional: an
/// unavailable home directory must not stop the app from starting or prevent
/// the user from changing the language for this session.
pub struct PresentationState {
    inner: Mutex<PresentationInner>,
    /// Serializes file publication and the in-memory commit. Without it, two
    /// concurrent trusted-bootstrap invokes could leave disk and memory with
    /// different last writers.
    update_gate: Mutex<()>,
    path: Option<PathBuf>,
}

impl PresentationState {
    fn new(
        preference: LocalePreference,
        locale: PresentationLocale,
        persisted: bool,
        path: Option<PathBuf>,
    ) -> Self {
        Self {
            inner: Mutex::new(PresentationInner {
                preference,
                locale,
                persisted,
            }),
            update_gate: Mutex::new(()),
            path,
        }
    }

    pub fn snapshot(&self) -> PresentationSnapshot {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        PresentationSnapshot {
            preference: inner.preference,
            locale: inner.locale,
            persisted: inner.persisted,
        }
    }

    fn set(
        &self,
        preference: LocalePreference,
        system_languages: &[String],
    ) -> Result<PresentationSnapshot, String> {
        let _update_gate = self
            .update_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let locale = resolve_preference(preference, system_languages)?;
        // Do not hold the state mutex while touching the file system. The
        // native menu/tray update happens after this method returns too.
        let persisted = match self.path.as_ref() {
            Some(path) => match write_preference(path, preference) {
                Ok(()) => true,
                Err(_) => {
                    eprintln!(
                        "[dsh-desktop] controller locale preference could not be persisted; using session preference"
                    );
                    false
                }
            },
            None => false,
        };
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.preference = preference;
        inner.locale = locale;
        inner.persisted = persisted;
        Ok(PresentationSnapshot {
            preference,
            locale,
            persisted,
        })
    }
}

/// Initialize native presentation state before the tray/menu are built so a
/// previous manual language choice applies even before Svelte has loaded.
pub fn init(app: &AppHandle) {
    let (path, preference, persisted) = match preference_path(app) {
        Ok(path) => match read_preference(&path) {
            Ok(Some(preference)) => (Some(path), preference, true),
            Ok(None) => (Some(path), LocalePreference::System, false),
            Err(_) => {
                eprintln!(
                    "[dsh-desktop] controller locale preference was invalid; following the system language"
                );
                (Some(path), LocalePreference::System, false)
            }
        },
        Err(_) => {
            eprintln!(
                "[dsh-desktop] controller locale storage unavailable; following the system language"
            );
            (None, LocalePreference::System, false)
        }
    };
    let locale = resolve_preference(preference, &[]).unwrap_or(PresentationLocale::English);
    app.manage(PresentationState::new(preference, locale, persisted, path));
    apply_window_titles(app);
}

fn preference_path(app: &AppHandle) -> Result<PathBuf, String> {
    let paths = crate::paths::resolve(app)?;
    // Use the same checked, private home as the Harness. This is intentionally
    // performed before joining our child path so neither final component can
    // be a symlink/reparse point.
    crate::secure_fs::ensure_private_dir(&paths.dsh_home)?;
    let state_dir = paths.dsh_home.join(".desktop-tools");
    crate::secure_fs::ensure_private_dir(&state_dir)?;
    Ok(state_dir.join("controller-locale.json"))
}

fn read_preference(path: &std::path::Path) -> Result<Option<LocalePreference>, String> {
    let Some(bytes) = crate::secure_fs::read_bounded(path, PREFERENCE_MAX_BYTES as u64)? else {
        return Ok(None);
    };
    let stored: StoredPreference = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid controller locale preference".to_string())?;
    if stored.schema != PREFERENCE_SCHEMA {
        return Err("unsupported controller locale preference schema".to_string());
    }
    Ok(Some(stored.preference))
}

fn write_preference(path: &std::path::Path, preference: LocalePreference) -> Result<(), String> {
    let bytes = serde_json::to_vec(&StoredPreference {
        schema: PREFERENCE_SCHEMA,
        preference,
    })
    .map_err(|_| "cannot encode controller locale preference".to_string())?;
    crate::secure_fs::atomic_write(path, &bytes, PREFERENCE_MAX_BYTES)
}

fn is_valid_system_language_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_SYSTEM_LANGUAGE_TAG_BYTES
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn resolve_preference(
    preference: LocalePreference,
    system_languages: &[String],
) -> Result<PresentationLocale, String> {
    match preference {
        LocalePreference::SimplifiedChinese => Ok(PresentationLocale::SimplifiedChinese),
        LocalePreference::English => Ok(PresentationLocale::English),
        LocalePreference::System => {
            if system_languages.len() > MAX_SYSTEM_LANGUAGE_TAGS {
                return Err("system language list is too large".to_string());
            }
            if system_languages.is_empty() {
                return Ok(native_system_locale());
            }
            if system_languages
                .iter()
                .any(|tag| !is_valid_system_language_tag(tag))
            {
                return Err("system language list is invalid".to_string());
            }
            Ok(locale_from_language_tags(
                system_languages.iter().map(String::as_str),
            ))
        }
    }
}

/// The one language mapping used by the browser, native tray, and macOS menu.
/// Traditional Chinese deliberately falls back to English rather than showing
/// Simplified Chinese labels under the wrong writing system.
pub(crate) fn locale_from_language_tags<I, S>(languages: I) -> PresentationLocale
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    languages
        .into_iter()
        .find_map(|tag| {
            let normalized = tag.as_ref().trim().replace('_', "-").to_ascii_lowercase();
            if matches!(normalized.as_str(), "zh" | "zh-cn" | "zh-sg" | "zh-hans")
                || normalized.starts_with("zh-hans-")
            {
                Some(PresentationLocale::SimplifiedChinese)
            } else if normalized == "en" || normalized.starts_with("en-") {
                Some(PresentationLocale::English)
            } else {
                None
            }
        })
        .unwrap_or(PresentationLocale::English)
}

fn native_system_locale() -> PresentationLocale {
    locale_from_language_tags(native_system_language_tags())
}

#[cfg(target_os = "macos")]
fn native_system_language_tags() -> Vec<String> {
    use objc2_foundation::NSLocale;

    NSLocale::preferredLanguages()
        .iter()
        .map(|language| language.to_string())
        .collect()
}

#[cfg(windows)]
fn native_system_language_tags() -> Vec<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    // Windows guarantees that the locale name, including its NUL terminator,
    // fits in LOCALE_NAME_MAX_LENGTH (85 UTF-16 code units).
    let mut buffer = [0_u16; 85];
    // SAFETY: buffer is writable, correctly sized, and remains live for the
    // duration of the documented Win32 call.
    let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 1 || length as usize > buffer.len() {
        return Vec::new();
    }
    String::from_utf16(&buffer[..length as usize - 1])
        .map(|locale| vec![locale])
        .unwrap_or_default()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn native_system_language_tags() -> Vec<String> {
    for name in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = std::env::var(name) else {
            continue;
        };
        let values = value
            .split(':')
            .map(|item| item.split_once('.').map_or(item, |(language, _)| language))
            .map(|item| item.split_once('@').map_or(item, |(language, _)| language))
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !values.is_empty() {
            return values;
        }
    }
    Vec::new()
}

#[cfg(not(any(target_os = "macos", windows, unix)))]
fn native_system_language_tags() -> Vec<String> {
    Vec::new()
}

pub(crate) fn current_locale(app: &AppHandle) -> PresentationLocale {
    app.try_state::<PresentationState>()
        .map(|state| state.snapshot().locale)
        .unwrap_or(PresentationLocale::English)
}

pub(crate) fn controller_window_title(locale: PresentationLocale) -> &'static str {
    match locale {
        PresentationLocale::SimplifiedChinese => "DSH Desktop — 控制器",
        PresentationLocale::English => "DSH Desktop — Controller",
    }
}

pub(crate) fn harness_window_title(locale: PresentationLocale) -> &'static str {
    match locale {
        PresentationLocale::SimplifiedChinese => "DSH Desktop — Harness 控制台",
        PresentationLocale::English => "DSH Desktop — Harness",
    }
}

pub(crate) fn harness_window_title_for(app: &AppHandle) -> &'static str {
    harness_window_title(current_locale(app))
}

pub(crate) fn apply_window_titles(app: &AppHandle) {
    let locale = current_locale(app);
    if let Some(window) = app.get_webview_window("bootstrap") {
        let _ = window.set_title(controller_window_title(locale));
    }
    if let Some(window) = app.get_webview_window("harness") {
        let _ = window.set_title(harness_window_title(locale));
    }
}

/// Called only after a validated preference change. Snapshot the state before
/// updating native UI objects so no mutex is held across a main-thread menu or
/// window operation.
pub(crate) fn apply_native_presentation(app: &AppHandle) {
    let locale = current_locale(app);
    apply_window_titles(app);
    crate::tray::update_locale(app, locale);
    #[cfg(target_os = "macos")]
    crate::app_menu::apply_locale(app, locale);
}

#[tauri::command]
pub fn get_presentation_locale(
    presentation: tauri::State<'_, PresentationState>,
) -> PresentationSnapshot {
    presentation.snapshot()
}

#[tauri::command]
pub fn set_presentation_locale(
    preference: LocalePreference,
    system_languages: Vec<String>,
    app: AppHandle,
    presentation: tauri::State<'_, PresentationState>,
) -> Result<PresentationSnapshot, String> {
    let snapshot = presentation.set(preference, &system_languages)?;
    apply_native_presentation(&app);
    Ok(snapshot)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshd-presentation-{name}-{}",
            crate::secure_fs::random_suffix().unwrap()
        ))
    }

    #[test]
    fn language_mapping_matches_the_controller_contract() {
        assert_eq!(
            locale_from_language_tags(["fr-FR", "zh_Hans_CN", "en-US"]),
            PresentationLocale::SimplifiedChinese
        );
        assert_eq!(
            locale_from_language_tags(["ja-JP", "en_GB", "zh-CN"]),
            PresentationLocale::English
        );
        assert_eq!(
            locale_from_language_tags(["zh-Hant-TW", "fr-FR"]),
            PresentationLocale::English
        );
    }

    #[test]
    fn manual_preference_cannot_be_overridden_by_system_languages() {
        assert_eq!(
            resolve_preference(LocalePreference::English, &["zh-CN".to_string()]).unwrap(),
            PresentationLocale::English
        );
        assert_eq!(
            resolve_preference(LocalePreference::SimplifiedChinese, &["en-US".to_string()])
                .unwrap(),
            PresentationLocale::SimplifiedChinese
        );
    }

    #[test]
    fn system_language_input_is_bounded_and_strict() {
        assert!(resolve_preference(LocalePreference::System, &["en-US".repeat(20)]).is_err());
        assert!(resolve_preference(LocalePreference::System, &["en US".to_string()]).is_err());
        assert!(resolve_preference(
            LocalePreference::System,
            &std::iter::repeat_n("en-US".to_string(), MAX_SYSTEM_LANGUAGE_TAGS + 1)
                .collect::<Vec<_>>(),
        )
        .is_err());
    }

    #[test]
    fn stored_preference_is_small_strict_and_recoverable() {
        let root = test_dir("storage");
        crate::secure_fs::ensure_private_dir(&root).unwrap();
        let path = root.join("controller-locale.json");
        write_preference(&path, LocalePreference::English).unwrap();
        assert_eq!(
            read_preference(&path).unwrap(),
            Some(LocalePreference::English)
        );

        crate::secure_fs::atomic_write(
            &path,
            br#"{"schema":1,"preference":"en","extra":true}"#,
            128,
        )
        .unwrap();
        assert!(read_preference(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preference_storage_refuses_a_symlinked_leaf() {
        use std::os::unix::fs::symlink;

        let root = test_dir("symlink");
        crate::secure_fs::ensure_private_dir(&root).unwrap();
        let target = root.join("target.json");
        std::fs::write(&target, br#"{"schema":1,"preference":"en"}"#).unwrap();
        let path = root.join("controller-locale.json");
        symlink(&target, &path).unwrap();

        assert!(read_preference(&path).is_err());
        assert!(write_preference(&path, LocalePreference::SimplifiedChinese).is_err());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            br#"{"schema":1,"preference":"en"}"#
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn titles_localize_the_window_role_but_keep_the_product_name() {
        assert_eq!(
            controller_window_title(PresentationLocale::English),
            "DSH Desktop — Controller"
        );
        assert_eq!(
            controller_window_title(PresentationLocale::SimplifiedChinese),
            "DSH Desktop — 控制器"
        );
        assert!(harness_window_title(PresentationLocale::English).starts_with("DSH Desktop"));
    }
}
