//! System tray: the desktop is tray-owned — closing windows hides them and
//! the Harness keeps running until the user quits from the tray menu.
//!
//! Fallback policy: if the tray cannot be created (headless Linux, missing
//! appindicator, or DSH_FORCE_NO_TRAY=1 for tests), the app keeps the normal
//! per-platform close semantics — closing the last window must never leave a
//! live-but-unreachable application behind.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Wry,
};

use crate::{harness::Status, presentation::PresentationLocale};

struct TrayItems {
    open_controller: MenuItem<Wry>,
    open_harness: MenuItem<Wry>,
    restart: MenuItem<Wry>,
    stop: MenuItem<Wry>,
    status: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

pub struct TrayState {
    pub available: AtomicBool,
    dispatch: Option<Arc<TrayDispatch>>,
}

/// Coalesces asynchronous native-menu updates. Tauri's `MenuItem` setters
/// wait for the main event loop, so calling them synchronously from a tray
/// callback would deadlock that loop. A single worker owns updates, and the
/// one-slot pending value means a burst can neither create unbounded threads
/// nor make the native menu lag behind its latest known state.
struct TrayDispatch {
    pending: Mutex<Option<TrayModel>>,
    wake: Condvar,
}

impl TrayDispatch {
    /// Start the one bounded menu-updater. If the OS cannot allocate this
    /// non-critical helper, the freshly built tray stays in its conservative
    /// idle state: lifecycle entries are disabled and every click still
    /// re-checks live Harness state.
    fn start(items: TrayItems) -> Option<Arc<Self>> {
        let dispatch = Arc::new(Self {
            pending: Mutex::new(None),
            wake: Condvar::new(),
        });
        let worker = dispatch.clone();
        match std::thread::Builder::new()
            .name("dsh-tray-menu".to_string())
            .spawn(move || worker.run(items))
        {
            Ok(_) => Some(dispatch),
            Err(error) => {
                eprintln!("[dsh-desktop] tray menu updater unavailable: {error}");
                None
            }
        }
    }

    fn submit(&self, model: TrayModel) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Replacement (rather than a queue) intentionally coalesces noisy
        // state publications to the latest complete menu model.
        *pending = Some(model);
        self.wake.notify_one();
    }

    fn run(self: Arc<Self>, items: TrayItems) {
        loop {
            let model = {
                let mut pending = self
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                loop {
                    if let Some(model) = pending.take() {
                        break model;
                    }
                    pending = self
                        .wake
                        .wait(pending)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            };
            apply_items(&items, &model);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrayModel {
    open_controller: &'static str,
    open_harness: &'static str,
    restart: &'static str,
    stop: &'static str,
    status: String,
    quit: &'static str,
    open_harness_enabled: bool,
    restart_enabled: bool,
    stop_enabled: bool,
}

pub fn available(app: &AppHandle) -> bool {
    app.try_state::<TrayState>()
        .map(|t| t.available.load(Ordering::SeqCst))
        .unwrap_or(false)
}

fn status_text(locale: PresentationLocale, status: Status) -> &'static str {
    match (locale, status) {
        (PresentationLocale::SimplifiedChinese, Status::Idle) => "等待启动",
        (PresentationLocale::SimplifiedChinese, Status::Starting) => "启动中…",
        (PresentationLocale::SimplifiedChinese, Status::Running) => "运行中",
        (PresentationLocale::SimplifiedChinese, Status::Stopping) => "停止中…",
        (PresentationLocale::SimplifiedChinese, Status::Stopped) => "已停止",
        (PresentationLocale::SimplifiedChinese, Status::Crashed) => "进程异常退出",
        (PresentationLocale::English, Status::Idle) => "Waiting to start",
        (PresentationLocale::English, Status::Starting) => "Starting…",
        (PresentationLocale::English, Status::Running) => "Running",
        (PresentationLocale::English, Status::Stopping) => "Stopping…",
        (PresentationLocale::English, Status::Stopped) => "Stopped",
        (PresentationLocale::English, Status::Crashed) => "Process crashed",
    }
}

/// Pure mapping from the harness state to the complete tray menu. Keeping the
/// permissions here means a stale menu click is safe to re-check at dispatch.
fn menu_model(locale: PresentationLocale, status: Status, harness_ready: bool) -> TrayModel {
    let (restart, restart_enabled, stop_enabled) = match status {
        Status::Running => match locale {
            PresentationLocale::SimplifiedChinese => ("重新启动 Harness", true, true),
            PresentationLocale::English => ("Restart Harness", true, true),
        },
        Status::Stopped => match locale {
            PresentationLocale::SimplifiedChinese => ("启动 Harness", true, false),
            PresentationLocale::English => ("Start Harness", true, false),
        },
        Status::Crashed => match locale {
            PresentationLocale::SimplifiedChinese => ("重新启动 Harness", true, false),
            PresentationLocale::English => ("Restart Harness", true, false),
        },
        // The sidecar has an in-flight lifecycle request. Do not queue a
        // second one from the tray; the controller follows the same policy.
        Status::Idle | Status::Starting | Status::Stopping => match locale {
            PresentationLocale::SimplifiedChinese => ("启动 Harness", false, false),
            PresentationLocale::English => ("Start Harness", false, false),
        },
    };
    match locale {
        PresentationLocale::SimplifiedChinese => TrayModel {
            open_controller: "打开控制器",
            open_harness: "打开 Harness",
            restart,
            stop: "停止 Harness",
            status: format!("Harness：{}", status_text(locale, status)),
            quit: "退出 DSH Desktop",
            open_harness_enabled: status == Status::Running && harness_ready,
            restart_enabled,
            stop_enabled,
        },
        PresentationLocale::English => TrayModel {
            open_controller: "Open Controller",
            open_harness: "Open Harness",
            restart,
            stop: "Stop Harness",
            status: format!("Harness: {}", status_text(locale, status)),
            quit: "Quit DSH Desktop",
            open_harness_enabled: status == Status::Running && harness_ready,
            restart_enabled,
            stop_enabled,
        },
    }
}

fn current_harness_state(app: &AppHandle) -> (Status, bool) {
    app.try_state::<crate::harness::Runtime>()
        .map(|runtime| {
            let state = runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let ready = state.status == Status::Running
                && state
                    .url
                    .as_deref()
                    .is_some_and(crate::harness::is_valid_readiness_url);
            (state.status, ready)
        })
        .unwrap_or((Status::Idle, false))
}

fn apply_items(items: &TrayItems, model: &TrayModel) {
    // MenuItem::set_* internally marshals to Tauri's main thread.
    let _ = items.open_controller.set_text(model.open_controller);
    let _ = items.open_harness.set_text(model.open_harness);
    let _ = items.open_harness.set_enabled(model.open_harness_enabled);
    let _ = items.restart.set_text(model.restart);
    let _ = items.restart.set_enabled(model.restart_enabled);
    let _ = items.stop.set_text(model.stop);
    let _ = items.stop.set_enabled(model.stop_enabled);
    let _ = items.status.set_text(&model.status);
    let _ = items.quit.set_text(model.quit);
}

fn apply_model(app: &AppHandle, model: &TrayModel) {
    let Some(tray) = app.try_state::<TrayState>() else {
        return;
    };
    let Some(dispatch) = tray.dispatch.as_ref() else {
        return;
    };
    dispatch.submit(model.clone());
}

/// Update tray state from the Harness' single snapshot publication path.
/// Safe to call from any reader/watcher thread.
pub fn update_status(app: &AppHandle, status: Status, harness_ready: bool) {
    let locale = crate::presentation::current_locale(app);
    apply_model(app, &menu_model(locale, status, harness_ready));
}

/// Update all tray labels after a user-visible language change.
pub(crate) fn update_locale(app: &AppHandle, locale: PresentationLocale) {
    let (status, harness_ready) = current_harness_state(app);
    apply_model(app, &menu_model(locale, status, harness_ready));
}

fn show_controller(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("bootstrap") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn show_harness_or_controller(app: &AppHandle) {
    let Some(runtime) = app.try_state::<crate::harness::Runtime>() else {
        show_controller(app);
        return;
    };
    let (status, url) = {
        let state = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state.status, state.url.clone())
    };
    // A menu item can become stale between refresh and click. Only a current,
    // validated readiness URL can open the zero-IPC Harness window.
    if status == Status::Running {
        if let Some(url) = url {
            if crate::harness::is_valid_readiness_url(&url) {
                crate::harness::open_harness_window(app, &url);
                return;
            }
        }
    }
    show_controller(app);
}

fn restart_from_tray(app: &AppHandle) {
    let (status, _) = current_harness_state(app);
    if !matches!(status, Status::Running | Status::Stopped | Status::Crashed) {
        return;
    }
    if let Err(error) = crate::harness::request_restart(app) {
        eprintln!("[dsh-desktop] tray restart failed: {error}");
    }
}

fn stop_from_tray(app: &AppHandle) {
    // Re-check the live state rather than trusting the enabled bit from a
    // previously rendered native menu.
    if current_harness_state(app).0 != Status::Running {
        return;
    }
    if let Err(error) = crate::harness::request_shutdown(app) {
        eprintln!("[dsh-desktop] tray stop failed: {error}");
    }
}

/// Build the tray. Never fails the app: on error the tray stays unavailable
/// and `TrayState::available` stays false so window-close keeps normal
/// semantics.
pub fn init(app: &AppHandle) {
    let enabled = std::env::var("DSH_FORCE_NO_TRAY").is_err();
    let locale = crate::presentation::current_locale(app);
    let mut state = TrayState {
        available: AtomicBool::new(false),
        dispatch: None,
    };
    if enabled {
        match build_tray(app, locale) {
            Ok((_tray, items)) => {
                state.available.store(true, Ordering::SeqCst);
                state.dispatch = TrayDispatch::start(items);
            }
            Err(error) => {
                eprintln!(
                    "[dsh-desktop] tray unavailable, falling back to normal close semantics: {error}"
                );
            }
        }
    } else {
        eprintln!("[dsh-desktop] DSH_FORCE_NO_TRAY set: tray disabled (test mode)");
    }
    app.manage(state);
    let (status, harness_ready) = current_harness_state(app);
    update_status(app, status, harness_ready);
}

fn build_tray(
    app: &AppHandle,
    locale: PresentationLocale,
) -> tauri::Result<(tauri::tray::TrayIcon<Wry>, TrayItems)> {
    let model = menu_model(locale, Status::Idle, false);
    let controller = MenuItem::with_id(
        app,
        "open-controller",
        model.open_controller,
        true,
        None::<&str>,
    )?;
    let harness = MenuItem::with_id(
        app,
        "open-harness",
        model.open_harness,
        model.open_harness_enabled,
        None::<&str>,
    )?;
    let restart = MenuItem::with_id(
        app,
        "restart-harness",
        model.restart,
        model.restart_enabled,
        None::<&str>,
    )?;
    let stop = MenuItem::with_id(
        app,
        "stop-harness",
        model.stop,
        model.stop_enabled,
        None::<&str>,
    )?;
    let status = MenuItem::with_id(app, "harness-status", model.status, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", model.quit, true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &controller,
            &harness,
            &restart,
            &stop,
            &sep1,
            &status,
            &sep2,
            &quit,
        ],
    )?;

    let builder = TrayIconBuilder::with_id("main-tray")
        .icon(tauri::include_image!("icons/tray-template.png"))
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open-controller" => show_controller(app),
            "open-harness" => show_harness_or_controller(app),
            "restart-harness" => restart_from_tray(app),
            "stop-harness" => stop_from_tray(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Linux emits no click events, so the explicit menu actions stay
            // the portable path. Other platforms get the same safe behavior.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_harness_or_controller(tray.app_handle());
            }
        });

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let builder = builder.show_menu_on_left_click(false);
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);

    let tray = builder.build(app)?;
    Ok((
        tray,
        TrayItems {
            open_controller: controller,
            open_harness: harness,
            restart,
            stop,
            status,
            quit,
        },
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn chinese_model_localizes_all_actions_and_gates_lifecycle() {
        let running = menu_model(PresentationLocale::SimplifiedChinese, Status::Running, true);
        assert_eq!(running.open_controller, "打开控制器");
        assert_eq!(running.open_harness, "打开 Harness");
        assert_eq!(running.restart, "重新启动 Harness");
        assert_eq!(running.stop, "停止 Harness");
        assert_eq!(running.status, "Harness：运行中");
        assert_eq!(running.quit, "退出 DSH Desktop");
        assert!(running.open_harness_enabled);
        assert!(running.restart_enabled);
        assert!(running.stop_enabled);

        let stopped = menu_model(
            PresentationLocale::SimplifiedChinese,
            Status::Stopped,
            false,
        );
        assert_eq!(stopped.restart, "启动 Harness");
        assert!(stopped.restart_enabled);
        assert!(!stopped.stop_enabled);
    }

    #[test]
    fn english_model_exposes_only_safe_actions_during_transitions() {
        let starting = menu_model(PresentationLocale::English, Status::Starting, false);
        assert_eq!(starting.open_controller, "Open Controller");
        assert_eq!(starting.open_harness, "Open Harness");
        assert_eq!(starting.status, "Harness: Starting…");
        assert!(!starting.open_harness_enabled);
        assert!(!starting.restart_enabled);
        assert!(!starting.stop_enabled);

        let crashed = menu_model(PresentationLocale::English, Status::Crashed, false);
        assert_eq!(crashed.restart, "Restart Harness");
        assert!(crashed.restart_enabled);
        assert!(!crashed.open_harness_enabled);
        assert!(!crashed.stop_enabled);

        let unready = menu_model(PresentationLocale::English, Status::Running, false);
        assert!(!unready.open_harness_enabled);
        assert_eq!(unready.quit, "Quit DSH Desktop");
    }

    #[test]
    fn lifecycle_gate_never_queues_conflicting_tray_mutations() {
        for status in [Status::Idle, Status::Starting, Status::Stopping] {
            let model = menu_model(PresentationLocale::English, status, false);
            assert!(!model.restart_enabled, "{status:?} must not restart");
            assert!(!model.stop_enabled, "{status:?} must not stop");
            assert!(!model.open_harness_enabled, "{status:?} must not open");
        }

        for status in [Status::Stopped, Status::Crashed] {
            let model = menu_model(PresentationLocale::English, status, false);
            assert!(model.restart_enabled, "{status:?} must allow restart");
            assert!(!model.stop_enabled, "{status:?} has nothing to stop");
            assert!(!model.open_harness_enabled, "{status:?} has no ready URL");
        }

        let ready = menu_model(PresentationLocale::English, Status::Running, true);
        assert!(ready.restart_enabled);
        assert!(ready.stop_enabled);
        assert!(ready.open_harness_enabled);
    }
}
