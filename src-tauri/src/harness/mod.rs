//! Tauri-side supervisor for the dsh-sidecar process.
//!
//! The Tauri core never launches Node itself — it only supervises the
//! sidecar over NDJSON (the sidecar owns the Node/Harness tree). This keeps
//! the privilege surface tiny and the protocol identical for CLI tooling.

use crate::paths::{resolve, RuntimePaths};
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

const MAX_LOGS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Idle,
    Starting,
    Running,
    Stopping,
    Stopped,
    Crashed,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Versions {
    pub desktop: String,
    pub harness: String,
    pub node: String,
    pub sidecar: String,
}

#[derive(Default)]
pub struct SharedState {
    pub status: Status,
    pub url: Option<String>,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    pub logs: Vec<(String, String)>,
    pub versions: Versions,
    pub dsh_home: Option<String>,
}

impl Runtime {
    /// The resolved runtime paths (available after `init()`).
    pub fn paths(&self) -> Option<crate::paths::RuntimePaths> {
        self.paths.clone()
    }
}

pub struct Runtime {
    pub state: Arc<Mutex<SharedState>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    shutting_down: Arc<AtomicBool>,
    restart_attempts: Arc<AtomicU32>,
    paths: Option<RuntimePaths>,
    /// Bumped on every sidecar launch; watcher threads hold the value they
    /// started with and must never touch a newer generation's resources.
    gen: Arc<AtomicU64>,
    /// Bumped on every user-initiated restart; cancels queued auto-restarts.
    restart_gen: Arc<AtomicU64>,
}

/// The origin the currently open harness window was created for. Restarts may
/// change the port; the window must be recreated in that case because its
/// on_navigation guard is bound to the creation origin.
#[derive(Default)]
pub struct WindowOrigin(pub Mutex<Option<String>>);

/// Crash auto-restart policy: up to this many consecutive attempts with
/// exponential backoff, then give up and surface the error.
const MAX_RESTART_ATTEMPTS: u32 = 3;

/// NDJSON command ids — one place to change protocol bookkeeping.
pub(crate) const CMD_ID_START: u64 = 1;
pub(crate) const CMD_ID_STATUS: u64 = 99;
pub(crate) const CMD_ID_RESTART: u64 = 100;
pub(crate) const CMD_ID_SHUTDOWN: u64 = 101;
pub(crate) const CMD_ID_EXIT_SHUTDOWN: u64 = 900;

/// Send a raw command line through the sidecar stdin, if available.
fn send_restart(stdin: &Arc<Mutex<Option<ChildStdin>>>) {
    let mut stdin = stdin
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(stdin) = stdin.as_mut() {
        let _ = writeln!(stdin, "{{\"id\":{CMD_ID_RESTART},\"command\":\"restart\"}}");
        let _ = stdin.flush();
    }
}

/// Schedule a crash auto-restart with exponential backoff (1s, 2s, 4s…).
fn schedule_auto_restart(
    state: &Arc<Mutex<SharedState>>,
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    shutting_down: &Arc<AtomicBool>,
    restart_gen: &Arc<AtomicU64>,
    attempts: u32,
) {
    let state_c = state.clone();
    let stdin_c = stdin.clone();
    let shutting_down_c = shutting_down.clone();
    let restart_gen_c = restart_gen.clone();
    let my_gen = restart_gen.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let backoff = std::time::Duration::from_secs(1u64 << (attempts.saturating_sub(1).min(3)));
        std::thread::sleep(backoff);
        // A user-initiated restart during the backoff supersedes this one.
        if shutting_down_c.load(Ordering::SeqCst) || restart_gen_c.load(Ordering::SeqCst) != my_gen
        {
            return;
        }
        {
            let mut s = state_c
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            s.status = Status::Starting;
        }
        send_restart(&stdin_c);
    });
}

fn log_line(state: &Mutex<SharedState>, stream: &str, line: &str) {
    let mut s = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    s.logs.push((stream.to_string(), line.to_string()));
    if s.logs.len() > MAX_LOGS {
        let excess = s.logs.len() - MAX_LOGS;
        s.logs.drain(..excess);
    }
}

fn set_error(state: &Mutex<SharedState>, message: impl Into<String>) {
    let mut s = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    s.last_error = Some(message.into());
    s.status = Status::Crashed;
    s.url = None;
    s.pid = None;
}

/// Ask the sidecar for a status refresh (carries the real pid).
fn refresh_pid(stdin: &Arc<Mutex<Option<ChildStdin>>>) {
    let mut stdin = stdin
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(stdin) = stdin.as_mut() {
        let _ = writeln!(stdin, "{{\"id\":{CMD_ID_STATUS},\"command\":\"status\"}}");
        let _ = stdin.flush();
    }
}

/// Strict readiness-URL validation: the exact shape the sidecar emits.
/// http + host 127.0.0.1 + path "/" + explicit port 1..=65535, no userinfo,
/// no query, no fragment. Any other shape is not a Harness readiness URL.
pub(crate) fn is_valid_readiness_url(url: &str) -> bool {
    let Ok(parsed) = tauri::Url::parse(url) else {
        return false;
    };
    parsed.scheme() == "http"
        && parsed.host_str() == Some("127.0.0.1")
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && matches!(parsed.port(), Some(port) if (1..=65535).contains(&port))
}

/// Navigation guard for the remote webview: any path/query is fine (the
/// Harness UI routes internally), but the origin must be exactly the
/// readiness origin captured at window creation — never another local port.
fn same_origin(candidate: &tauri::Url, origin: &tauri::Url) -> bool {
    candidate.scheme() == origin.scheme()
        && candidate.host_str() == origin.host_str()
        && candidate.port() == origin.port()
}

/// Open (or focus) the harness window. The remote webview may only navigate
/// within the readiness origin — even with zero IPC permissions, a stray page
/// link must not turn the window into a general-purpose browser or a jumper
/// into other local services.
pub(crate) fn open_harness_window(app: &AppHandle, url: &str) {
    if !is_valid_readiness_url(url) {
        return;
    }
    let Ok(parsed) = tauri::Url::parse(url) else {
        return;
    };
    let origin = parsed.origin().ascii_serialization();

    // Same origin as the current window → refresh + show/focus (the
    // navigation guard allows same-origin navigations).
    let window_origin = app.try_state::<WindowOrigin>();
    let current = window_origin.as_ref().and_then(|w| {
        w.0.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    });
    if current.as_deref() == Some(origin.as_str()) {
        let app_in = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(win) = app_in.get_webview_window("harness") {
                let _ = win.navigate(parsed.clone());
                let _ = win.show();
                let _ = win.set_focus();
            }
        });
        return;
    }

    // Different origin (restart picked a new port): the old window's
    // navigation guard would block the new port, so recreate the window.
    let Ok(origin_parsed) = tauri::Url::parse(&origin) else {
        return; // unreachable: is_valid_readiness_url passed above
    };
    if let Some(window_origin) = window_origin {
        *window_origin
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(origin.clone());
    }
    let app_in = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app_in.get_webview_window("harness") {
            // destroy() bypasses CloseRequested (which hides instead of closing).
            let _ = win.destroy();
        }
        let _ = tauri::WebviewWindowBuilder::new(
            &app_in,
            "harness",
            tauri::WebviewUrl::External(parsed),
        )
        .title("DeepSeek Harness")
        .inner_size(1280.0, 800.0)
        .min_inner_size(960.0, 600.0)
        .on_navigation(move |candidate| same_origin(candidate, &origin_parsed))
        .build();
    });
}

/// Side effects requested by the pure state transition, executed by the
/// caller (which owns AppHandle, stdin and threads).
#[derive(Debug, PartialEq, Eq)]
enum SideEffect {
    OpenWindow(String),
    RefreshPid,
    ScheduleAutoRestart(u32),
}

/// Pure core of handle_event: mutates SharedState only and returns the side
/// effects the caller must execute. Unit-testable without Tauri machinery.
fn apply_state_event(
    state: &Mutex<SharedState>,
    shutting_down: &AtomicBool,
    restart_attempts: &AtomicU32,
    ev: &Value,
) -> Vec<SideEffect> {
    let mut effects = Vec::new();
    let ty = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match ty {
        "ack" => {
            if ev.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                let msg = ev
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown sidecar error");
                set_error(state, msg.to_string());
            }
        }
        "sidecar" => {
            if let Some(v) = ev.get("version").and_then(|v| v.as_str()) {
                state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .versions
                    .sidecar = v.to_string();
            }
        }
        "starting" => {
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .status = Status::Starting;
        }
        "ready" => {
            let url = ev
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // The sidecar derives this URL from the readiness line, but the
            // shell must not trust it blindly: a malformed ready must never
            // fake a Running state with an unusable URL.
            if !is_valid_readiness_url(&url) {
                log_line(
                    state,
                    "sidecar",
                    &format!("ignoring malformed ready URL: {url:?}"),
                );
                return effects;
            }
            {
                let mut s = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                s.status = Status::Running;
                s.url = Some(url.clone());
                s.last_error = None;
            }
            // A successful boot resets the crash counter.
            restart_attempts.store(0, Ordering::SeqCst);
            effects.push(SideEffect::RefreshPid);
            effects.push(SideEffect::OpenWindow(url));
        }
        "stopping" => {
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .status = Status::Stopping;
        }
        "stopped" => {
            {
                let mut s = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                s.status = Status::Stopped;
                s.pid = None;
                // The port may change on the next boot; never keep a dead URL.
                s.url = None;
            }
            effects.push(SideEffect::RefreshPid);
        }
        "crashed" => {
            let code = ev.get("code").and_then(|v| v.as_i64());
            // The sidecar attaches a human-readable `message` for diagnosed
            // kills (readiness timeout, unresponsive). Prefer it over the
            // generic exit-code wording — a bare "(code 9)" tells the user
            // nothing about why the restart is happening.
            let detail = ev.get("message").and_then(|v| v.as_str());
            let describe = || match detail {
                Some(m) => format!("Harness 进程异常退出：{m}"),
                None => format!(
                    "Harness 进程异常退出 (code {})",
                    code.map(|c| c.to_string())
                        .unwrap_or_else(|| "unknown".into())
                ),
            };
            if shutting_down.load(Ordering::SeqCst) {
                return effects;
            }
            let attempts = restart_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempts <= MAX_RESTART_ATTEMPTS {
                {
                    let mut s = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    s.last_error = Some(format!(
                        "{}，正在自动重启（第 {attempts}/{MAX_RESTART_ATTEMPTS} 次）…",
                        describe()
                    ));
                    // During the backoff window the old URL is dead — surface
                    // "starting" instead of a stale "running" with a dead port.
                    s.status = Status::Starting;
                    s.url = None;
                    s.pid = None;
                }
                effects.push(SideEffect::ScheduleAutoRestart(attempts));
            } else {
                set_error(
                    state,
                    format!(
                        "{}；已连续崩溃 {MAX_RESTART_ATTEMPTS} 次，停止自动重启",
                        describe()
                    ),
                );
            }
        }
        "error" => {
            let msg = ev
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown sidecar error");
            // Transient diagnostics (unresponsive/readiness-timeout) arrive
            // BEFORE the kill sequence they announce. Flipping to Crashed
            // here would make the UI flash Crashed→Stopping→Starting while
            // the tree is still tearing down. Record the message, but only
            // take the fatal state when we are not mid-transition.
            let mut s = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            s.last_error = Some(msg.to_string());
            if s.status != Status::Stopping && s.status != Status::Starting {
                s.status = Status::Crashed;
                s.url = None;
                s.pid = None;
            }
        }
        "status" => {
            if let Some(pid) = ev.get("pid").and_then(|v| v.as_u64()) {
                state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pid = Some(pid as u32);
            }
        }
        "log" => {
            let stream = ev
                .get("stream")
                .and_then(|v| v.as_str())
                .unwrap_or("stdout");
            let line = ev.get("line").and_then(|v| v.as_str()).unwrap_or("");
            log_line(state, stream, line);
        }
        _ => {}
    }
    effects
}

fn handle_event(
    app: &AppHandle,
    state: &Arc<Mutex<SharedState>>,
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    shutting_down: &Arc<AtomicBool>,
    restart_attempts: &Arc<AtomicU32>,
    restart_gen: &Arc<AtomicU64>,
    ev: &Value,
) {
    // Log lines flow by the thousands during agent runs; the snapshot payload
    // does not contain logs, so broadcasting it per line is pure IPC noise.
    if ev.get("type").and_then(|v| v.as_str()) == Some("log") {
        let stream = ev
            .get("stream")
            .and_then(|v| v.as_str())
            .unwrap_or("stdout");
        let line = ev.get("line").and_then(|v| v.as_str()).unwrap_or("");
        log_line(state, stream, line);
        return;
    }

    for effect in apply_state_event(state, shutting_down, restart_attempts, ev) {
        match effect {
            SideEffect::OpenWindow(url) => open_harness_window(app, &url),
            SideEffect::RefreshPid => refresh_pid(stdin),
            SideEffect::ScheduleAutoRestart(attempts) => {
                schedule_auto_restart(state, stdin, shutting_down, restart_gen, attempts)
            }
        }
    }
    publish_snapshot(app, state);
}

/// Single snapshot publication channel: emits to the bootstrap window AND
/// updates the tray status line. Every state mutation path must end here so
/// the tray can never go stale (watcher, fail_init, command errors included).
pub(crate) fn publish_snapshot(app: &AppHandle, state: &Arc<Mutex<SharedState>>) {
    let snapshot = snapshot_payload(state);
    if let Some(status) = snapshot.get("status").and_then(|v| v.as_str()) {
        crate::tray::update_status(app, &crate::tray::status_label(status));
    }
    let _ = app.emit_to("bootstrap", "harness-event", &snapshot);
}

pub fn snapshot_payload(state: &Arc<Mutex<SharedState>>) -> Value {
    let s = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    serde_json::json!({
        "status": s.status,
        "url": s.url,
        "pid": s.pid,
        "lastError": s.last_error,
        "versions": s.versions,
        "dshHome": s.dsh_home,
    })
}

/// Reset the crash auto-restart counter (user-initiated restarts start fresh).
pub fn reset_restart_attempts(runtime: &Runtime) {
    runtime.restart_attempts.store(0, Ordering::SeqCst);
}

fn read_versions(paths: &RuntimePaths) -> Versions {
    let manifest = paths.harness_dir.join("runtime-manifest.json");
    let (harness, node) = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|v| {
            (
                v.get("harnessVersion")
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                v.get("nodeVersion")
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            )
        })
        .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));
    Versions {
        desktop: env!("CARGO_PKG_VERSION").to_string(),
        harness,
        node,
        sidecar: "unknown".to_string(),
    }
}

/// The Arc handles shared by the Runtime and its threads, bundled so the
/// init helpers stay under clippy's argument budget.
struct RuntimeArcs {
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    shutting_down: Arc<AtomicBool>,
    restart_attempts: Arc<AtomicU32>,
    gen: Arc<AtomicU64>,
    restart_gen: Arc<AtomicU64>,
}

/// Shared init-failure path: manage an errored Runtime so the UI has a state
/// to render instead of a dead window.
fn fail_init(app: &AppHandle, arcs: RuntimeArcs, paths: Option<RuntimePaths>, message: String) {
    let state = Arc::new(Mutex::new(SharedState::default()));
    set_error(&state, message);
    app.manage(Runtime {
        state: state.clone(),
        stdin: arcs.stdin,
        child: arcs.child,
        shutting_down: arcs.shutting_down,
        restart_attempts: arcs.restart_attempts,
        paths,
        gen: arcs.gen,
        restart_gen: arcs.restart_gen,
    });
    // fail_init historically emitted nothing — the tray/UI must see it too.
    publish_snapshot(app, &state);
}

/// Spawn the sidecar process and wire reader/watcher threads. The Runtime
/// must already be managed; on success its stdin/child arcs are populated.
fn launch_sidecar(app: &AppHandle, runtime: &Runtime, paths: &RuntimePaths) -> Result<(), String> {
    // New generation: any watcher/reader from an older launch must not touch
    // this launch's child/stdin slots.
    let my_gen = runtime.gen.fetch_add(1, Ordering::SeqCst) + 1;
    let spawn_result = Command::new(&paths.sidecar)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let (mut child, stdin, stdout, stderr) = match spawn_result {
        Ok(mut c) => {
            let stdin = c.stdin.take();
            let stdout = c.stdout.take();
            let stderr = c.stderr.take();
            (Some(c), stdin, stdout, stderr)
        }
        Err(e) => {
            return Err(format!(
                "无法启动 sidecar ({}): {e} — 请先运行 `pnpm runtime:all`",
                paths.sidecar.display()
            ));
        }
    };

    if let Some(stdin) = stdin {
        *runtime
            .stdin
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(stdin);
    }
    if let Some(child) = child.take() {
        *runtime
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(child);
    }

    // Sidecar death watcher: a sidecar exit without an intentional app
    // shutdown is surfaced even if no final NDJSON event was received.
    {
        let child_c = runtime.child.clone();
        let state_c = runtime.state.clone();
        let stdin_c = runtime.stdin.clone();
        let shutting_down_c = runtime.shutting_down.clone();
        let gen_c = runtime.gen.clone();
        let app_c = app.clone();
        std::thread::spawn(move || loop {
            // A respawn supersedes us: never observe or mutate the slots of a
            // newer generation's sidecar.
            if gen_c.load(Ordering::SeqCst) != my_gen {
                return;
            }
            let exited = {
                let mut child = child_c
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => Some(status),
                        Ok(None) | Err(_) => None,
                    },
                    None => return,
                }
            };

            if let Some(status) = exited {
                if !shutting_down_c.load(Ordering::SeqCst) && gen_c.load(Ordering::SeqCst) == my_gen
                {
                    let _ = stdin_c
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take();
                    let code = status
                        .code()
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    {
                        let mut s = state_c
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        s.last_error = Some(format!("sidecar 进程意外退出 (code {code})"));
                        s.status = Status::Crashed;
                        s.pid = None;
                        s.url = None;
                    }
                    publish_snapshot(&app_c, &state_c);
                }
                return;
            }

            std::thread::sleep(std::time::Duration::from_millis(500));
        });
    }

    // Reader threads: stdout = NDJSON events, stderr = plain log lines.
    if let Some(stdout) = stdout {
        let state_c = runtime.state.clone();
        let stdin_c = runtime.stdin.clone();
        let app_c = app.clone();
        let shutting_down_c = runtime.shutting_down.clone();
        let attempts_c = runtime.restart_attempts.clone();
        let restart_gen_c = runtime.restart_gen.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match serde_json::from_str::<Value>(&line) {
                    Ok(ev) => handle_event(
                        &app_c,
                        &state_c,
                        &stdin_c,
                        &shutting_down_c,
                        &attempts_c,
                        &restart_gen_c,
                        &ev,
                    ),
                    Err(_) => log_line(&state_c, "sidecar", &line),
                }
            }
        });
    }
    if let Some(stderr) = stderr {
        let state_c = runtime.state.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log_line(&state_c, "sidecar", &line);
            }
        });
    }

    Ok(())
}

/// Send the NDJSON `start` command for the bundled runtime.
fn start_harness(runtime: &Runtime, paths: &RuntimePaths) -> Result<(), String> {
    if !paths.node.exists() || !paths.harness_dir.join("node_modules").exists() {
        return Err(
            "runtime 未就绪（缺少 node 或 harness/node_modules）— 请先运行 `pnpm runtime:all`"
                .to_string(),
        );
    }
    let dsh_bin = paths
        .harness_dir
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
    let cmd = serde_json::json!({
        "id": CMD_ID_START,
        "command": "start",
        "node": paths.node,
        "script": dsh_bin,
        "args": ["web", "--host", "127.0.0.1", "--port", "0"],
        "cwd": paths.harness_dir,
        // DSH_HOME: the harness' own data root. DSH_TELEMETRY_DISABLED:
        // upstream dsh honors any non-empty value by disabling the
        // session-telemetry row — a community wrapper defaults to OFF.
        "env": { "DSH_HOME": paths.dsh_home, "DSH_TELEMETRY_DISABLED": "1" },
    });
    send_raw(runtime, &cmd)?;
    runtime
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .status = Status::Starting;
    Ok(())
}

/// Spawn the sidecar, wire the reader thread, and auto-start the Harness.
pub fn init(app: &AppHandle) {
    let stdin_arc: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(None));
    let child_arc: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let shutting_down = Arc::new(AtomicBool::new(false));
    let restart_attempts = Arc::new(AtomicU32::new(0));
    let gen = Arc::new(AtomicU64::new(0));
    let restart_gen = Arc::new(AtomicU64::new(0));
    app.manage(WindowOrigin::default());
    let arcs = RuntimeArcs {
        stdin: stdin_arc.clone(),
        child: child_arc.clone(),
        shutting_down: shutting_down.clone(),
        restart_attempts: restart_attempts.clone(),
        gen: gen.clone(),
        restart_gen: restart_gen.clone(),
    };

    let paths = match resolve(app) {
        Ok(paths) => paths,
        Err(e) => {
            fail_init(app, arcs, None, e);
            return;
        }
    };

    if let Err(e) = std::fs::create_dir_all(&paths.dsh_home) {
        let msg = format!("无法创建数据目录 {}: {e}", paths.dsh_home.display());
        fail_init(app, arcs, Some(paths), msg);
        return;
    }

    match std::fs::symlink_metadata(&paths.dsh_home) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let msg = format!("数据目录 {} 不能是符号链接", paths.dsh_home.display());
            fail_init(app, arcs, Some(paths), msg);
            return;
        }
        Ok(_) => {}
        Err(e) => {
            let msg = format!("无法检查数据目录 {}: {e}", paths.dsh_home.display());
            fail_init(app, arcs, Some(paths), msg);
            return;
        }
    }

    #[cfg(unix)]
    if let Err(e) = std::fs::set_permissions(
        &paths.dsh_home,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    ) {
        let msg = format!("无法设置数据目录权限 {}: {e}", paths.dsh_home.display());
        fail_init(app, arcs, Some(paths), msg);
        return;
    }

    // Fallback initialization of the user preset root: the import path
    // creates it on demand; this only guarantees the root exists from first
    // boot so upstream discovery and the settings-page roster never start
    // from a missing directory. Best-effort and silent — any real problem
    // surfaces through the import/validation commands instead.
    let _ = std::fs::create_dir_all(crate::preset::user_preset_root(&paths.dsh_home));
    // Compositions can hold secrets: match DSH_HOME's 0700 for the root
    // itself (best-effort, same silence as the mkdir above).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            crate::preset::user_preset_root(&paths.dsh_home),
            std::fs::Permissions::from_mode(0o700),
        );
    }

    let versions = read_versions(&paths);
    let state = Arc::new(Mutex::new(SharedState {
        status: Status::Idle,
        versions,
        dsh_home: Some(paths.dsh_home.display().to_string()),
        ..Default::default()
    }));

    app.manage(Runtime {
        state: state.clone(),
        stdin: stdin_arc,
        child: child_arc,
        shutting_down: shutting_down.clone(),
        restart_attempts: restart_attempts.clone(),
        paths: Some(paths.clone()),
        gen: gen.clone(),
        restart_gen: restart_gen.clone(),
    });
    let runtime = app.state::<Runtime>();

    if let Err(e) = launch_sidecar(app, &runtime, &paths) {
        set_error(&state, e);
        publish_snapshot(app, &state);
        return;
    }
    if let Err(e) = start_harness(&runtime, &paths) {
        set_error(&state, e);
        publish_snapshot(app, &state);
    }
}

/// Unified restart entry (tray menu + bootstrap button): sidecar alive → send
/// restart command; sidecar dead → respawn the whole chain. Publishes state.
pub fn request_restart(app: &AppHandle) -> Result<(), String> {
    let runtime = app.state::<Runtime>();
    // App teardown has begun (shutdown_blocking sets this before dropping the
    // stdin pipe): a respawn now would be EOF-killed moments later — refuse
    // instead of spawning an orphan. Closes the tray-restart vs exit race.
    if runtime.shutting_down.load(Ordering::SeqCst) {
        return Err("应用正在退出，无法重启".to_string());
    }
    // Supersede any queued crash auto-restart: the user is in control now.
    runtime.restart_gen.fetch_add(1, Ordering::SeqCst);
    if !child_alive(&runtime) {
        let result = respawn_sidecar(app);
        if let Err(e) = &result {
            let mut s = runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            s.last_error = Some(e.clone());
            s.status = Status::Crashed;
        } else {
            reset_restart_attempts(&runtime);
            let mut s = runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            s.last_error = None;
        }
        publish_snapshot(app, &runtime.state);
        return result;
    }

    let result = send_raw(
        &runtime,
        &serde_json::json!({"id": CMD_ID_RESTART, "command": "restart"}),
    );
    if let Err(e) = &result {
        let mut s = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        s.last_error = Some(e.clone());
        s.status = Status::Crashed;
    } else {
        reset_restart_attempts(&runtime);
        let mut s = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        s.last_error = None;
        s.status = Status::Starting;
    }
    publish_snapshot(app, &runtime.state);
    result
}

/// Re-launch the sidecar after it died unexpectedly (user presses the restart
/// button). Resets the crash counter and re-sends the start command.
pub fn respawn_sidecar(app: &AppHandle) -> Result<(), String> {
    let runtime = app.state::<Runtime>();
    // shutting_down is a one-way latch set by shutdown_blocking before it
    // drops the stdin pipe. A respawn racing app exit would spawn a sidecar
    // that shutdown_blocking kills immediately via stdin EOF — never do
    // that. Check FIRST: respawn must not reset the latch either (a
    // store(false) here would re-open the race it is closing).
    if runtime.shutting_down.load(Ordering::SeqCst) {
        return Err("应用正在退出，无法重启".to_string());
    }
    if child_alive(&runtime) {
        return Ok(());
    }
    let paths = runtime
        .paths
        .clone()
        .ok_or_else(|| "运行时路径不可用".to_string())?;
    runtime.restart_attempts.store(0, Ordering::SeqCst);
    runtime
        .stdin
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    launch_sidecar(app, &runtime, &paths)?;
    start_harness(&runtime, &paths)
}

pub fn send_raw(runtime: &Runtime, cmd: &Value) -> Result<(), String> {
    let mut stdin = runtime
        .stdin
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stdin = stdin
        .as_mut()
        .ok_or_else(|| "sidecar stdin unavailable".to_string())?;
    writeln!(stdin, "{cmd}").map_err(|e| format!("failed to write to sidecar: {e}"))?;
    stdin
        .flush()
        .map_err(|e| format!("failed to flush sidecar command: {e}"))
}

pub fn child_alive(runtime: &Runtime) -> bool {
    match runtime
        .child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
    {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

/// Blocking teardown used on app exit: polite shutdown, then reap the sidecar.
/// The Stopped-wait matches the sidecar's own graceful window
/// (DSH_SHUTDOWN_GRACE_MS, default 10s) so the harness actually gets its full
/// chance to exit cleanly before the stdin-EOF force path takes over. The wait
/// is skipped entirely when there is no live child (init failed, sidecar
/// already dead) or the state already settled.
pub fn shutdown_blocking(app: &AppHandle) {
    let runtime = app.state::<Runtime>();
    runtime.shutting_down.store(true, Ordering::SeqCst);
    let _ = send_raw(
        &runtime,
        &serde_json::json!({"id": CMD_ID_EXIT_SHUTDOWN, "command": "shutdown"}),
    );

    let grace = std::env::var("DSH_SHUTDOWN_GRACE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(10));
    let stopped_deadline = std::time::Instant::now() + grace;
    while child_alive(&runtime) && std::time::Instant::now() < stopped_deadline {
        let status = runtime
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .status;
        if status == Status::Stopped || status == Status::Crashed {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    *runtime
        .stdin
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;

    let exit_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while child_alive(&runtime) && std::time::Instant::now() < exit_deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if child_alive(&runtime) {
        if let Some(child) = runtime
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh() -> (Mutex<SharedState>, AtomicBool, AtomicU32) {
        (
            Mutex::new(SharedState {
                status: Status::Idle,
                ..Default::default()
            }),
            AtomicBool::new(false),
            AtomicU32::new(0),
        )
    }

    #[test]
    fn ready_event_sets_running_and_requests_window() {
        let (state, shutting_down, attempts) = fresh();
        attempts.store(2, Ordering::SeqCst);
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"ready","url":"http://127.0.0.1:41234"}),
        );
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.url.as_deref(), Some("http://127.0.0.1:41234"));
        assert_eq!(s.last_error, None);
        drop(s);
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
        assert_eq!(
            effects,
            vec![
                SideEffect::RefreshPid,
                SideEffect::OpenWindow("http://127.0.0.1:41234".to_string())
            ]
        );
    }

    #[test]
    fn crashed_event_schedules_backoff_restart() {
        let (state, shutting_down, attempts) = fresh();
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .status = Status::Running;
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .url = Some("http://127.0.0.1:9999".to_string());
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"crashed","code":1}),
        );
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(s.last_error.as_deref().unwrap().contains("自动重启"));
        // No stale "running" with a dead port during the backoff window.
        assert_eq!(s.status, Status::Starting);
        assert_eq!(s.url, None);
        assert_eq!(s.pid, None);
        drop(s);
        assert_eq!(effects, vec![SideEffect::ScheduleAutoRestart(1)]);
    }

    #[test]
    fn crashed_event_gives_up_after_max_attempts() {
        let (state, shutting_down, attempts) = fresh();
        attempts.store(MAX_RESTART_ATTEMPTS, Ordering::SeqCst);
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"crashed","code":1}),
        );
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(s.status, Status::Crashed);
        assert!(s.last_error.as_deref().unwrap().contains("停止自动重启"));
        drop(s);
        assert!(effects.is_empty());
    }

    #[test]
    fn crashed_event_suppressed_while_shutting_down() {
        let (state, shutting_down, attempts) = fresh();
        shutting_down.store(true, Ordering::SeqCst);
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"crashed","code":1}),
        );
        assert!(effects.is_empty());
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(s.status, Status::Idle);
        assert_eq!(s.last_error, None);
    }

    #[test]
    fn crashed_event_with_message_surfaces_the_sidecar_text() {
        // The heartbeat's crashed carries `message` — the shell must prefer it
        // over the generic "(code N)" wording so "unresponsive" is actually
        // visible to the user.
        let (state, shutting_down, attempts) = fresh();
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"crashed","code":9,"message":"killed after health checks failed (unresponsive)"}),
        );
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let text = s.last_error.as_deref().unwrap();
        assert!(
            text.contains("killed after health checks failed (unresponsive)"),
            "message must be surfaced: {text}"
        );
        assert!(
            !text.contains("(code"),
            "message must REPLACE the exit-code wording, not append to it: {text}"
        );
        assert!(text.contains("自动重启"), "restart wording kept: {text}");
        drop(s);
        assert_eq!(effects, vec![SideEffect::ScheduleAutoRestart(1)]);
    }

    #[test]
    fn error_event_during_stopping_does_not_flip_status() {
        // The sidecar emits error/unresponsive BEFORE the kill sequence it
        // announces. While stopping, the error must be recorded but must not
        // flash Crashed (the UI would bounce Crashed→Stopping→Starting).
        let (state, shutting_down, attempts) = fresh();
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .status = Status::Stopping;
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"error","code":"unresponsive","message":"dsh web did not answer health probes; killing the tree"}),
        );
        assert!(effects.is_empty());
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(s.status, Status::Stopping, "status must not flip");
        assert!(s.last_error.as_deref().unwrap().contains("health probes"));
    }

    #[test]
    fn error_event_from_idle_still_takes_crashed() {
        // The fatal-state behavior is preserved for errors that are NOT part
        // of a stop sequence (spawn failures etc.).
        let (state, shutting_down, attempts) = fresh();
        apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"error","code":"spawn-failed","message":"boom"}),
        );
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(s.status, Status::Crashed);
    }

    #[test]
    fn crashed_event_without_message_keeps_exit_code_wording() {
        let (state, shutting_down, attempts) = fresh();
        apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"crashed","code":7}),
        );
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            s.last_error.as_deref().unwrap().contains("code 7"),
            "exit-code wording must remain the fallback"
        );
    }

    #[test]
    fn failed_ack_sets_crashed_with_message() {
        let (state, shutting_down, attempts) = fresh();
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"ack","id":100,"ok":false,"error":"nothing to restart"}),
        );
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(s.status, Status::Crashed);
        assert_eq!(s.last_error.as_deref(), Some("nothing to restart"));
        drop(s);
        assert!(effects.is_empty());
    }

    #[test]
    fn stopped_event_clears_pid_and_refreshes() {
        let (state, shutting_down, attempts) = fresh();
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pid = Some(4242);
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"stopped","code":0}),
        );
        let s = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(s.status, Status::Stopped);
        assert_eq!(s.pid, None);
        drop(s);
        assert_eq!(effects, vec![SideEffect::RefreshPid]);
    }

    #[test]
    fn status_event_updates_pid() {
        let (state, shutting_down, attempts) = fresh();
        apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"status","id":99,"state":"running","pid":123}),
        );
        assert_eq!(
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pid,
            Some(123)
        );
    }

    #[test]
    fn unknown_event_has_no_effect() {
        let (state, shutting_down, attempts) = fresh();
        let effects = apply_state_event(
            &state,
            &shutting_down,
            &attempts,
            &json!({"type":"mystery"}),
        );
        assert!(effects.is_empty());
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;
        use serde_json::json;

        // Any stream of arbitrary NDJSON events must never panic the state
        // machine, and must keep the core invariants:
        //  * Running implies a known URL,
        //  * the log ring never exceeds its cap.
        proptest! {
            #[test]
            fn apply_state_event_invariants(events in prop::collection::vec(
                prop_oneof![
                    Just(json!({"type": "ready", "url": "http://127.0.0.1:41234"})),
                    Just(json!({"type": "starting"})),
                    Just(json!({"type": "stopping"})),
                    Just(json!({"type": "stopped", "code": 0})),
                    Just(json!({"type": "crashed", "code": 1})),
                    Just(json!({"type": "error", "message": "boom"})),
                    Just(json!({"type": "status", "id": 99, "state": "running", "pid": 123})),
                    Just(json!({"type": "log", "stream": "stdout", "line": "x"})),
                    Just(json!({"type": "ack", "id": 1, "ok": true})),
                    Just(json!({"type": "ack", "id": 1, "ok": false, "error": "nope"})),
                    Just(json!({"type": "sidecar", "version": "0.1.0"})),
                    Just(json!({"type": "mystery"})),
                    Just(json!({"type": 42, "whatever": [1, 2, 3]})),
                    Just(json!({"type": "ready"})), // missing url
                    Just(json!({"type": "crashed", "code": null})),
                ],
                0..64
            )) {
                let (state, shutting_down, attempts) = fresh();
                for ev in &events {
                    let _ = apply_state_event(&state, &shutting_down, &attempts, ev);
                }
                let s = state.lock().unwrap();
                if s.status == Status::Running {
                    prop_assert!(s.url.is_some(), "Running without URL after {:?}", events);
                }
                prop_assert!(s.logs.len() <= MAX_LOGS);
            }

            #[test]
            fn apply_state_event_url_sanity(urls in prop::collection::vec(any::<String>(), 0..16)) {
                let (state, shutting_down, attempts) = fresh();
                for url in &urls {
                    let _ = apply_state_event(
                        &state,
                        &shutting_down,
                        &attempts,
                        &json!({"type": "ready", "url": url}),
                    );
                }
                let s = state.lock().unwrap();
                if s.status == Status::Running {
                    prop_assert!(s.url.as_deref().is_some_and(is_valid_readiness_url));
                }
            }
        }
    }

    #[test]
    fn validates_readiness_urls() {
        assert!(is_valid_readiness_url("http://127.0.0.1:41234"));
        assert!(is_valid_readiness_url("http://127.0.0.1:1/"));
        assert!(!is_valid_readiness_url("http://127.0.0.1:0"));
        assert!(!is_valid_readiness_url("http://127.0.0.1:65536"));
        assert!(!is_valid_readiness_url("http://127.0.0.1"));
        assert!(!is_valid_readiness_url("http://localhost:41234"));
        assert!(!is_valid_readiness_url("https://127.0.0.1:41234"));
        assert!(!is_valid_readiness_url("http://127.0.0.1:41234/some/path"));
        assert!(!is_valid_readiness_url("http://127.0.0.1:41234?q=1"));
        assert!(!is_valid_readiness_url("http://user@127.0.0.1:41234"));
        assert!(!is_valid_readiness_url("http://192.168.1.5:41234"));
        assert!(!is_valid_readiness_url("http://127.0.0.1:41234#frag"));
        assert!(!is_valid_readiness_url("not a url"));
        assert!(!is_valid_readiness_url(""));
    }
}
