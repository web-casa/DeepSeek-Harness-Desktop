//! Localized native application menu for macOS.
//!
//! The menu mirrors Tauri's default role-only surface. It intentionally adds
//! no custom command, menu event, frontend IPC, reload, or developer action.

use crate::presentation::PresentationLocale;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MenuLabels {
    about_prefix: &'static str,
    services: &'static str,
    hide_prefix: &'static str,
    hide_others: &'static str,
    quit_prefix: &'static str,
    file: &'static str,
    close_window: &'static str,
    edit: &'static str,
    undo: &'static str,
    redo: &'static str,
    cut: &'static str,
    copy: &'static str,
    paste: &'static str,
    select_all: &'static str,
    view: &'static str,
    fullscreen: &'static str,
    window: &'static str,
    minimize: &'static str,
    zoom: &'static str,
    help: &'static str,
}

const ENGLISH: MenuLabels = MenuLabels {
    about_prefix: "About",
    services: "Services",
    hide_prefix: "Hide",
    hide_others: "Hide Others",
    quit_prefix: "Quit",
    file: "File",
    close_window: "Close Window",
    edit: "Edit",
    undo: "Undo",
    redo: "Redo",
    cut: "Cut",
    copy: "Copy",
    paste: "Paste",
    select_all: "Select All",
    view: "View",
    fullscreen: "Enter Full Screen",
    window: "Window",
    minimize: "Minimize",
    zoom: "Zoom",
    help: "Help",
};

const SIMPLIFIED_CHINESE: MenuLabels = MenuLabels {
    about_prefix: "关于",
    services: "服务",
    hide_prefix: "隐藏",
    hide_others: "隐藏其他",
    quit_prefix: "退出",
    file: "文件",
    close_window: "关闭窗口",
    edit: "编辑",
    undo: "撤销",
    redo: "重做",
    cut: "剪切",
    copy: "拷贝",
    paste: "粘贴",
    select_all: "全选",
    view: "显示",
    fullscreen: "进入全屏幕",
    window: "窗口",
    minimize: "最小化",
    zoom: "缩放",
    help: "帮助",
};

const fn labels(locale: PresentationLocale) -> &'static MenuLabels {
    match locale {
        PresentationLocale::English => &ENGLISH,
        PresentationLocale::SimplifiedChinese => &SIMPLIFIED_CHINESE,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct MenuInstallFailure<E> {
    install: E,
    had_previous: bool,
    restore: Option<E>,
}

fn replace_with_fallback<T, E>(
    replacement: T,
    previous: Option<T>,
    mut apply: impl FnMut(T) -> Result<(), E>,
) -> Result<(), MenuInstallFailure<E>> {
    match apply(replacement) {
        Ok(()) => Ok(()),
        Err(install) => {
            let had_previous = previous.is_some();
            let restore = previous.and_then(|menu| apply(menu).err());
            Err(MenuInstallFailure {
                install,
                had_previous,
                restore,
            })
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{labels, replace_with_fallback, MenuLabels, PresentationLocale};
    use tauri::{
        menu::{
            AboutMetadata, Menu, PredefinedMenuItem, Submenu, HELP_SUBMENU_ID, WINDOW_SUBMENU_ID,
        },
        AppHandle, Wry,
    };

    fn build_menu(app: &AppHandle, locale: PresentationLocale) -> tauri::Result<Menu<Wry>> {
        let labels = labels(locale);
        let package = app.package_info();
        let app_name = package.name.clone();
        let config = app.config();
        let about_metadata = AboutMetadata {
            name: Some(app_name.clone()),
            version: Some(package.version.to_string()),
            copyright: config.bundle.copyright.clone(),
            authors: config
                .bundle
                .publisher
                .clone()
                .map(|publisher| vec![publisher]),
            ..Default::default()
        };

        let about = format!("{} {app_name}", labels.about_prefix);
        let hide = format!("{} {app_name}", labels.hide_prefix);
        let quit = format!("{} {app_name}", labels.quit_prefix);

        let app_menu = Submenu::with_items(
            app,
            &app_name,
            true,
            &[
                &PredefinedMenuItem::about(app, Some(&about), Some(about_metadata))?,
                &PredefinedMenuItem::separator(app)?,
                &PredefinedMenuItem::services(app, Some(labels.services))?,
                &PredefinedMenuItem::separator(app)?,
                &PredefinedMenuItem::hide(app, Some(&hide))?,
                &PredefinedMenuItem::hide_others(app, Some(labels.hide_others))?,
                &PredefinedMenuItem::separator(app)?,
                &PredefinedMenuItem::quit(app, Some(&quit))?,
            ],
        )?;
        let file_menu = Submenu::with_items(
            app,
            labels.file,
            true,
            &[&PredefinedMenuItem::close_window(
                app,
                Some(labels.close_window),
            )?],
        )?;
        let edit_menu = Submenu::with_items(
            app,
            labels.edit,
            true,
            &[
                &PredefinedMenuItem::undo(app, Some(labels.undo))?,
                &PredefinedMenuItem::redo(app, Some(labels.redo))?,
                &PredefinedMenuItem::separator(app)?,
                &PredefinedMenuItem::cut(app, Some(labels.cut))?,
                &PredefinedMenuItem::copy(app, Some(labels.copy))?,
                &PredefinedMenuItem::paste(app, Some(labels.paste))?,
                &PredefinedMenuItem::select_all(app, Some(labels.select_all))?,
            ],
        )?;
        let view_menu = Submenu::with_items(
            app,
            labels.view,
            true,
            &[&PredefinedMenuItem::fullscreen(
                app,
                Some(labels.fullscreen),
            )?],
        )?;
        let window_menu = Submenu::with_id_and_items(
            app,
            WINDOW_SUBMENU_ID,
            labels.window,
            true,
            &[
                &PredefinedMenuItem::minimize(app, Some(labels.minimize))?,
                &PredefinedMenuItem::maximize(app, Some(labels.zoom))?,
                &PredefinedMenuItem::separator(app)?,
                &PredefinedMenuItem::close_window(app, Some(labels.close_window))?,
            ],
        )?;
        let help_menu = Submenu::with_id(app, HELP_SUBMENU_ID, labels.help, true)?;

        Menu::with_items(
            app,
            &[
                &app_menu,
                &file_menu,
                &edit_menu,
                &view_menu,
                &window_menu,
                &help_menu,
            ],
        )
    }

    pub(crate) fn apply_locale(app: &AppHandle, locale: PresentationLocale) {
        let menu = match build_menu(app, locale) {
            Ok(menu) => menu,
            Err(error) => {
                eprintln!(
                    "[dsh-desktop] localized macOS menu unavailable; keeping Tauri default: {error}"
                );
                return;
            }
        };
        let previous = app.menu();
        if let Err(failure) = replace_with_fallback(menu, previous, |candidate| {
            app.set_menu(candidate).map(|_| ())
        }) {
            match failure.restore {
                Some(restore) => eprintln!(
                    "[dsh-desktop] localized macOS menu install failed: {}; default menu restore also failed: {restore}",
                    failure.install
                ),
                None if failure.had_previous => eprintln!(
                    "[dsh-desktop] localized macOS menu install failed; restored Tauri default: {}",
                    failure.install
                ),
                None => eprintln!(
                    "[dsh-desktop] localized macOS menu install failed and no previous menu was available: {}",
                    failure.install
                ),
            }
        }
    }

    pub(crate) fn init(app: &AppHandle, locale: PresentationLocale) {
        apply_locale(app, locale);
    }
}

#[cfg(target_os = "macos")]
pub(crate) use macos::{apply_locale, init};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn selects_first_supported_preference_and_normalizes_separators() {
        assert_eq!(
            crate::presentation::locale_from_language_tags(["fr-FR", "zh_Hans_CN", "en-US"]),
            PresentationLocale::SimplifiedChinese
        );
        assert_eq!(
            crate::presentation::locale_from_language_tags(["ja-JP", "en_GB", "zh-CN"]),
            PresentationLocale::English
        );
    }

    #[test]
    fn traditional_chinese_does_not_select_simplified_labels() {
        assert_eq!(
            crate::presentation::locale_from_language_tags(["zh-Hant-TW", "en-US"]),
            PresentationLocale::English
        );
        assert_eq!(
            crate::presentation::locale_from_language_tags(["zh-TW", "zh-CN"]),
            PresentationLocale::SimplifiedChinese
        );
    }

    #[test]
    fn empty_or_unsupported_preferences_fall_back_to_english() {
        assert_eq!(
            crate::presentation::locale_from_language_tags(std::iter::empty::<&str>()),
            PresentationLocale::English
        );
        assert_eq!(
            crate::presentation::locale_from_language_tags(["fr-FR", "ja-JP"]),
            PresentationLocale::English
        );
    }

    #[test]
    fn label_sets_cover_the_complete_native_surface() {
        let english = labels(PresentationLocale::English);
        assert_eq!(english.file, "File");
        assert_eq!(english.close_window, "Close Window");
        assert_eq!(english.fullscreen, "Enter Full Screen");
        assert_eq!(english.help, "Help");

        let chinese = labels(PresentationLocale::SimplifiedChinese);
        assert_eq!(chinese.file, "文件");
        assert_eq!(chinese.close_window, "关闭窗口");
        assert_eq!(chinese.fullscreen, "进入全屏幕");
        assert_eq!(chinese.help, "帮助");
    }

    #[test]
    fn failed_replacement_restores_the_previous_menu() {
        let mut attempts = Vec::new();
        let result = replace_with_fallback("localized", Some("default"), |menu| {
            attempts.push(menu);
            if menu == "localized" {
                Err("install failed")
            } else {
                Ok(())
            }
        });
        assert_eq!(attempts, vec!["localized", "default"]);
        assert_eq!(
            result,
            Err(MenuInstallFailure {
                install: "install failed",
                had_previous: true,
                restore: None,
            })
        );
    }

    #[test]
    fn reports_both_replacement_and_restore_failures() {
        let result = replace_with_fallback("localized", Some("default"), Err);
        assert_eq!(
            result,
            Err(MenuInstallFailure {
                install: "localized",
                had_previous: true,
                restore: Some("default"),
            })
        );
    }

    #[test]
    fn reports_when_no_previous_menu_can_be_restored() {
        let result = replace_with_fallback("localized", None, Err);
        assert_eq!(
            result,
            Err(MenuInstallFailure {
                install: "localized",
                had_previous: false,
                restore: None,
            })
        );
    }
}
