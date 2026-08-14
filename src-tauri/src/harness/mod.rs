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
}

pub struct Runtime {
    pub state: Arc<Mutex<SharedState>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
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

fn open_harness_window(app: &AppHandle, url: &str) {
    let Ok(parsed) = tauri::Url::parse(url) else { return };
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
            .build();
        }
    });
}

fn handle_event(
    app: &AppHandle,
    state: &Arc<Mutex<SharedState>>,
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    ev: &Value,
) {
    let ty = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match ty {
        "sidecar" => {
            if let Some(v) = ev.get("version").and_then(|v| v.as_str()) {
                state.lock().unwrap().versions.sidecar = v.to_string();
            }
        }
        "starting" => {
            state.lock().unwrap().status = Status::Starting;
        }
        "ready" => {
            let url = ev.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            {
                let mut s = state.lock().unwrap();
                s.status = Status::Running;
                s.url = Some(url.clone());
                s.last_error = None;
            }
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
            let msg = ev
                .get("message")
                .and_then(|v| v.as_str())
                .map(|m| format!("Harness 进程异常退出 (code {code:?}): {m}"))
                .unwrap_or_else(|| format!("Harness 进程异常退出 (code {code:?})"));
            set_error(state, msg);
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
            let stream = ev.get("stream").and_then(|v| v.as_str()).unwrap_or("stdout");
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
    })
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

/// Spawn the sidecar, wire the reader thread, and auto-start the Harness.
pub fn init(app: &AppHandle) {
    let paths = resolve(app);

    let stdin_arc: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(None));
    let child_arc: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

    if let Err(e) = std::fs::create_dir_all(&paths.dsh_home) {
        let state = Arc::new(Mutex::new(SharedState::default()));
        set_error(&state, format!("无法创建数据目录 {}: {e}", paths.dsh_home.display()));
        app.manage(Runtime {
            state,
            stdin: stdin_arc.clone(),
            child: child_arc.clone(),
        });
        return;
    }

    let versions = read_versions(&paths);
    let state = Arc::new(Mutex::new(SharedState {
        status: Status::Idle,
        versions,
        ..Default::default()
    }));

    app.manage(Runtime {
        state: state.clone(),
        stdin: stdin_arc.clone(),
        child: child_arc.clone(),
    });
    let runtime = app.state::<Runtime>();

    // Spawn the sidecar.
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
            set_error(
                &state,
                format!(
                    "无法启动 sidecar ({}): {e} — 请先运行 `pnpm runtime:all`",
                    paths.sidecar.display()
                ),
            );
            (None, None, None, None)
        }
    };

    if let Some(stdin) = stdin {
        *stdin_arc.lock().unwrap() = Some(stdin);
    }
    if let Some(child) = child.take() {
        *child_arc.lock().unwrap() = Some(child);
    }

    // Reader threads: stdout = NDJSON events, stderr = plain log lines.
    if let Some(stdout) = stdout {
        let state_c = state.clone();
        let stdin_c = stdin_arc.clone();
        let app_c = app.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match serde_json::from_str::<Value>(&line) {
                    Ok(ev) => handle_event(&app_c, &state_c, &stdin_c, &ev),
                    Err(_) => log_line(&state_c, "sidecar", &line),
                }
            }
        });
    }
    if let Some(stderr) = stderr {
        let state_c = state.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log_line(&state_c, "sidecar", &line);
            }
        });
    }

    // Auto-start the Harness.
    if paths.node.exists() && paths.harness_dir.join("node_modules").exists() {
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
        send_raw(&runtime, &cmd);
        state.lock().unwrap().status = Status::Starting;
    } else {
        set_error(
            &state,
            "runtime 未就绪（缺少 node 或 harness/node_modules）— 请先运行 `pnpm runtime:all`",
        );
    }
}

pub fn send_raw(runtime: &Runtime, cmd: &Value) {
    let mut stdin = runtime.stdin.lock().unwrap();
    if let Some(stdin) = stdin.as_mut() {
        let _ = writeln!(stdin, "{cmd}");
        let _ = stdin.flush();
    }
}

/// Blocking teardown used on app exit: polite shutdown, then reap the sidecar.
pub fn shutdown_blocking(app: &AppHandle) {
    let runtime = app.state::<Runtime>();
    send_raw(&runtime, &serde_json::json!({"id": 900, "command": "shutdown"}));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    loop {
        if let Some(child) = runtime.child.lock().unwrap().as_mut() {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
        } else {
            break;
        }
        if std::time::Instant::now() >= deadline {
            if let Some(child) = runtime.child.lock().unwrap().as_mut() {
                let _ = child.kill();
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
