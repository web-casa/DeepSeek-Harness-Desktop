//! System tray: the desktop is tray-owned — closing windows hides them and
//! the harness keeps running until the user quits from the tray menu.
//!
//! Fallback policy: if the tray cannot be created (headless Linux, missing
//! appindicator, or DSH_FORCE_NO_TRAY=1 for tests), the app keeps the normal
//! per-platform close semantics — closing the last window must never leave a
//! live-but-unreachable application behind.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Wry,
};

pub struct TrayState {
    pub available: AtomicBool,
    status_item: Option<MenuItem<Wry>>,
}

pub fn available(app: &AppHandle) -> bool {
    app.try_state::<TrayState>()
        .map(|t| t.available.load(Ordering::SeqCst))
        .unwrap_or(false)
}

/// Pure mapping from the harness status to the tray status line. Tested.
pub fn status_label(status: &str) -> String {
    let label = match status {
        "idle" => "等待启动",
        "starting" => "启动中…",
        "running" => "运行中",
        "stopping" => "停止中…",
        "stopped" => "已停止",
        "crashed" => "启动失败",
        _ => status,
    };
    format!("Harness：{label}")
}

fn show_windows(app: &AppHandle) {
    for label in ["bootstrap", "harness"] {
        if let Some(win) = app.get_webview_window(label) {
            let _ = win.show();
            let _ = win.unminimize();
            let _ = win.set_focus();
        }
    }
}

/// Update the tray status line. Safe to call from any thread.
pub fn update_status(app: &AppHandle, label: &str) {
    let Some(tray) = app.try_state::<TrayState>() else {
        return;
    };
    let Some(item) = tray.status_item.clone() else {
        return;
    };
    let label = label.to_string();
    let _ = app.run_on_main_thread(move || {
        let _ = item.set_text(label);
    });
}

/// Build the tray. Never fails the app: on error the tray stays unavailable
/// and `TrayState::available` stays false so window-close keeps normal
/// semantics.
pub fn init(app: &AppHandle) {
    let available = std::env::var("DSH_FORCE_NO_TRAY").is_err();
    let mut state = TrayState {
        available: AtomicBool::new(false),
        status_item: None,
    };
    if available {
        match build_tray(app) {
            Ok((_tray, status_item)) => {
                state.available.store(true, Ordering::SeqCst);
                state.status_item = Some(status_item);
            }
            Err(e) => {
                eprintln!(
                    "[dsh-desktop] tray unavailable, falling back to normal close semantics: {e}"
                );
            }
        }
    } else {
        eprintln!("[dsh-desktop] DSH_FORCE_NO_TRAY set: tray disabled (test mode)");
    }
    app.manage(state);
}

fn build_tray(app: &AppHandle) -> tauri::Result<(tauri::tray::TrayIcon<Wry>, MenuItem<Wry>)> {
    let show_item = MenuItem::with_id(app, "show-harness", "显示 Harness", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "设置与日志", true, None::<&str>)?;
    let restart_item = MenuItem::with_id(app, "restart", "重新启动 Harness", true, None::<&str>)?;
    let status_item = MenuItem::with_id(app, "status", status_label("idle"), false, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出 DeepSeek Harness", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &settings_item,
            &restart_item,
            &sep1,
            &status_item,
            &sep2,
            &quit_item,
        ],
    )?;

    let builder = TrayIconBuilder::with_id("main-tray")
        .icon(tauri::include_image!("icons/tray-template.png"))
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show-harness" => {
                let runtime = app.state::<crate::harness::Runtime>();
                let url = runtime
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .url
                    .clone();
                match url {
                    Some(u) => crate::harness::open_harness_window(app, &u),
                    None => show_windows(app),
                }
            }
            "settings" => show_windows(app),
            "restart" => {
                if let Err(e) = crate::harness::request_restart(app) {
                    eprintln!("[dsh-desktop] tray restart failed: {e}");
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click Up shows the windows (mac/win). Linux emits no click
            // events, so the menu's "显示 Harness" is the fallback there.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_windows(tray.app_handle());
            }
        });

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let builder = builder.show_menu_on_left_click(false);
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);

    let tray = builder.build(app)?;
    Ok((tray, status_item))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn maps_status_to_tray_labels() {
        assert_eq!(status_label("running"), "Harness：运行中");
        assert_eq!(status_label("crashed"), "Harness：启动失败");
        assert_eq!(status_label("stopped"), "Harness：已停止");
        assert_eq!(status_label("starting"), "Harness：启动中…");
        assert_eq!(status_label("idle"), "Harness：等待启动");
        assert_eq!(status_label("mystery"), "Harness：mystery");
    }
}
