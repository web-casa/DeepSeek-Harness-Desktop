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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

pub struct Runtime {
    pub state: Arc<Mutex<SharedState>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    shutting_down: Arc<AtomicBool>,
    restart_attempts: Arc<AtomicU32>,
    paths: Option<RuntimePaths>,
}

/// Crash auto-restart policy: up to this many consecutive attempts with
/// exponential backoff, then give up and surface the error.
const MAX_RESTART_ATTEMPTS: u32 = 3;

/// Send a raw command line through the sidecar stdin, if available.
fn send_restart(stdin: &Arc<Mutex<Option<ChildStdin>>>) {
    let mut stdin = stdin.lock().unwrap();
    if let Some(stdin) = stdin.as_mut() {
        let _ = writeln!(stdin, "{{\"id\":100,\"command\":\"restart\"}}");
        let _ = stdin.flush();
    }
}

/// Schedule a crash auto-restart with exponential backoff (1s, 2s, 4s…).
fn schedule_auto_restart(
    state: &Arc<Mutex<SharedState>>,
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    shutting_down: &Arc<AtomicBool>,
    attempts: u32,
) {
    let state_c = state.clone();
    let stdin_c = stdin.clone();
    let shutting_down_c = shutting_down.clone();
    std::thread::spawn(move || {
        let backoff = std::time::Duration::from_secs(1u64 << (attempts.saturating_sub(1).min(3)));
        std::thread::sleep(backoff);
        if shutting_down_c.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut s = state_c.lock().unwrap();
            s.status = Status::Starting;
        }
        send_restart(&stdin_c);
    });
}

fn log_line(state: &Mutex<SharedState>, stream: &str, line: &str) {
    let mut s = state.lock().unwrap();
    s.logs.push((stream.to_string(), line.to_string()));
    if s.logs.len() > MAX_LOGS {
        let excess = s.logs.len() - MAX_LOGS;
        s.logs.drain(..excess);
    }
}

fn set_error(state: &Mutex<SharedState>, message: impl Into<String>) {
    let mut s = state.lock().unwrap();
    s.last_error = Some(message.into());
    s.status = Status::Crashed;
}

/// Ask the sidecar for a status refresh (carries the real pid).
fn refresh_pid(stdin: &Arc<Mutex<Option<ChildStdin>>>) {
    let mut stdin = stdin.lock().unwrap();
    if let Some(stdin) = stdin.as_mut() {
        let _ = writeln!(stdin, "{{\"id\":99,\"command\":\"status\"}}");
        let _ = stdin.flush();
    }
}

/// Open (or focus) the harness window. The remote webview may only navigate
/// to 127.0.0.1 — even with zero IPC permissions, a stray page link must not
/// be able to turn the window into a general-purpose browser.
pub(crate) fn open_harness_window(app: &AppHandle, url: &str) {
    let Ok(parsed) = tauri::Url::parse(url) else {
        return;
    };
    if parsed.host_str() != Some("127.0.0.1") {
        return;
    }
    let app = app.clone();
    let app_in = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app_in.get_webview_window("harness") {
            let _ = win.navigate(parsed.clone());
            let _ = win.show();
            let _ = win.set_focus();
        } else {
            let _ = tauri::WebviewWindowBuilder::new(
                &app_in,
                "harness",
                tauri::WebviewUrl::External(parsed),
            )
            .title("DeepSeek Harness")
            .inner_size(1280.0, 800.0)
            .min_inner_size(960.0, 600.0)
            .on_navigation(|url| url.host_str() == Some("127.0.0.1"))
            .build();
        }
    });
}

fn handle_event(
    app: &AppHandle,
    state: &Arc<Mutex<SharedState>>,
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    shutting_down: &Arc<AtomicBool>,
    restart_attempts: &Arc<AtomicU32>,
    ev: &Value,
) {
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
                state.lock().unwrap().versions.sidecar = v.to_string();
            }
        }
        "starting" => {
            state.lock().unwrap().status = Status::Starting;
        }
        "ready" => {
            let url = ev
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            {
                let mut s = state.lock().unwrap();
                s.status = Status::Running;
                s.url = Some(url.clone());
                s.last_error = None;
            }
            // A successful boot resets the crash counter.
            restart_attempts.store(0, Ordering::SeqCst);
            refresh_pid(stdin);
            open_harness_window(app, &url);
        }
        "stopping" => {
            state.lock().unwrap().status = Status::Stopping;
        }
        "stopped" => {
            {
                let mut s = state.lock().unwrap();
                s.status = Status::Stopped;
                s.pid = None;
            }
            refresh_pid(stdin);
        }
        "crashed" => {
            let code = ev.get("code").and_then(|v| v.as_i64());
            if shutting_down.load(Ordering::SeqCst) {
                return;
            }
            let attempts = restart_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempts <= MAX_RESTART_ATTEMPTS {
                {
                    let mut s = state.lock().unwrap();
                    s.last_error = Some(format!(
                        "Harness 进程异常退出 (code {code:?})，正在自动重启（第 {attempts}/{MAX_RESTART_ATTEMPTS} 次）…"
                    ));
                }
                schedule_auto_restart(state, stdin, shutting_down, attempts);
            } else {
                set_error(
                    state,
                    format!(
                        "Harness 进程异常退出 (code {code:?})；已连续崩溃 {MAX_RESTART_ATTEMPTS} 次，停止自动重启"
                    ),
                );
            }
        }
        "error" => {
            let msg = ev
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown sidecar error");
            set_error(state, msg.to_string());
        }
        "status" => {
            if let Some(pid) = ev.get("pid").and_then(|v| v.as_u64()) {
                state.lock().unwrap().pid = Some(pid as u32);
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
    let snapshot = snapshot_payload(state);
    let _ = app.emit_to("bootstrap", "harness-event", &snapshot);
}

pub fn snapshot_payload(state: &Arc<Mutex<SharedState>>) -> Value {
    let s = state.lock().unwrap();
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

/// Shared init-failure path: manage an errored Runtime so the UI has a state
/// to render instead of a dead window.
fn fail_init(
    app: &AppHandle,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    shutting_down: Arc<AtomicBool>,
    restart_attempts: Arc<AtomicU32>,
    paths: Option<RuntimePaths>,
    message: String,
) {
    let state = Arc::new(Mutex::new(SharedState::default()));
    set_error(&state, message);
    app.manage(Runtime {
        state,
        stdin,
        child,
        shutting_down,
        restart_attempts,
        paths,
    });
}

/// Spawn the sidecar process and wire reader/watcher threads. The Runtime
/// must already be managed; on success its stdin/child arcs are populated.
fn launch_sidecar(app: &AppHandle, runtime: &Runtime, paths: &RuntimePaths) -> Result<(), String> {
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
        *runtime.stdin.lock().unwrap() = Some(stdin);
    }
    if let Some(child) = child.take() {
        *runtime.child.lock().unwrap() = Some(child);
    }

    // Sidecar death watcher: a sidecar exit without an intentional app
    // shutdown is surfaced even if no final NDJSON event was received.
    {
        let child_c = runtime.child.clone();
        let state_c = runtime.state.clone();
        let stdin_c = runtime.stdin.clone();
        let shutting_down_c = runtime.shutting_down.clone();
        let app_c = app.clone();
        std::thread::spawn(move || loop {
            let exited = {
                let mut child = child_c.lock().unwrap();
                match child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => Some(status),
                        Ok(None) | Err(_) => None,
                    },
                    None => return,
                }
            };

            if let Some(status) = exited {
                if !shutting_down_c.load(Ordering::SeqCst) {
                    let _ = stdin_c.lock().unwrap().take();
                    let code = status
                        .code()
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    {
                        let mut s = state_c.lock().unwrap();
                        s.last_error = Some(format!("sidecar 进程意外退出 (code {code})"));
                        s.status = Status::Crashed;
                        s.pid = None;
                    }
                    let snapshot = snapshot_payload(&state_c);
                    let _ = app_c.emit_to("bootstrap", "harness-event", &snapshot);
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
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match serde_json::from_str::<Value>(&line) {
                    Ok(ev) => {
                        handle_event(&app_c, &state_c, &stdin_c, &shutting_down_c, &attempts_c, &ev)
                    }
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
            "runtime 未就绪（缺少 node 或 harness/node_modules）— 请先运行 `pnpm runtime:all`".to_string(),
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
        "id": 1,
        "command": "start",
        "node": paths.node,
        "script": dsh_bin,
        "args": ["web", "--host", "127.0.0.1", "--port", "0"],
        "cwd": paths.harness_dir,
        "env": { "DSH_HOME": paths.dsh_home },
    });
    send_raw(runtime, &cmd)?;
    runtime.state.lock().unwrap().status = Status::Starting;
    Ok(())
}

/// Spawn the sidecar, wire the reader thread, and auto-start the Harness.
pub fn init(app: &AppHandle) {
    let stdin_arc: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(None));
    let child_arc: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let shutting_down = Arc::new(AtomicBool::new(false));
    let restart_attempts = Arc::new(AtomicU32::new(0));

    let paths = match resolve(app) {
        Ok(paths) => paths,
        Err(e) => {
            fail_init(app, stdin_arc, child_arc, shutting_down, restart_attempts, None, e);
            return;
        }
    };

    if let Err(e) = std::fs::create_dir_all(&paths.dsh_home) {
        let msg = format!("无法创建数据目录 {}: {e}", paths.dsh_home.display());
        fail_init(app, stdin_arc, child_arc, shutting_down, restart_attempts, Some(paths), msg);
        return;
    }

    match std::fs::symlink_metadata(&paths.dsh_home) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let msg = format!("数据目录 {} 不能是符号链接", paths.dsh_home.display());
            fail_init(app, stdin_arc, child_arc, shutting_down, restart_attempts, Some(paths), msg);
            return;
        }
        Ok(_) => {}
        Err(e) => {
            let msg = format!("无法检查数据目录 {}: {e}", paths.dsh_home.display());
            fail_init(app, stdin_arc, child_arc, shutting_down, restart_attempts, Some(paths), msg);
            return;
        }
    }

    #[cfg(unix)]
    if let Err(e) = std::fs::set_permissions(
        &paths.dsh_home,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    ) {
        let msg = format!("无法设置数据目录权限 {}: {e}", paths.dsh_home.display());
        fail_init(app, stdin_arc, child_arc, shutting_down, restart_attempts, Some(paths), msg);
        return;
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
    });
    let runtime = app.state::<Runtime>();

    if let Err(e) = launch_sidecar(app, &runtime, &paths) {
        set_error(&state, e);
        return;
    }
    if let Err(e) = start_harness(&runtime, &paths) {
        set_error(&state, e);
    }
}

/// Re-launch the sidecar after it died unexpectedly (user presses the restart
/// button). Resets the crash counter and re-sends the start command.
pub fn respawn_sidecar(app: &AppHandle) -> Result<(), String> {
    let runtime = app.state::<Runtime>();
    if child_alive(&runtime) {
        return Ok(());
    }
    let paths = runtime
        .paths
        .clone()
        .ok_or_else(|| "运行时路径不可用".to_string())?;
    runtime.shutting_down.store(false, Ordering::SeqCst);
    runtime.restart_attempts.store(0, Ordering::SeqCst);
    runtime.stdin.lock().unwrap().take();
    launch_sidecar(app, &runtime, &paths)?;
    start_harness(&runtime, &paths)
}

pub fn send_raw(runtime: &Runtime, cmd: &Value) -> Result<(), String> {
    let mut stdin = runtime.stdin.lock().unwrap();
    let stdin = stdin
        .as_mut()
        .ok_or_else(|| "sidecar stdin unavailable".to_string())?;
    writeln!(stdin, "{cmd}").map_err(|e| format!("failed to write to sidecar: {e}"))?;
    stdin
        .flush()
        .map_err(|e| format!("failed to flush sidecar command: {e}"))
}

pub fn child_alive(runtime: &Runtime) -> bool {
    match runtime.child.lock().unwrap().as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

/// Blocking teardown used on app exit: polite shutdown, then reap the sidecar.
/// The Stopped-wait matches the sidecar's own graceful window
/// (DSH_SHUTDOWN_GRACE_MS, default 10s) so the harness actually gets its full
/// chance to exit cleanly before the stdin-EOF force path takes over.
pub fn shutdown_blocking(app: &AppHandle) {
    let runtime = app.state::<Runtime>();
    runtime.shutting_down.store(true, Ordering::SeqCst);
    let _ = send_raw(
        &runtime,
        &serde_json::json!({"id": 900, "command": "shutdown"}),
    );

    let grace = std::env::var("DSH_SHUTDOWN_GRACE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(10));
    let stopped_deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < stopped_deadline {
        if runtime.state.lock().unwrap().status == Status::Stopped {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    *runtime.stdin.lock().unwrap() = None;

    let exit_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while child_alive(&runtime) && std::time::Instant::now() < exit_deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if child_alive(&runtime) {
        if let Some(child) = runtime.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
}
