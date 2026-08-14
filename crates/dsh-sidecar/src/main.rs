#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! dsh-sidecar: a tiny process supervisor for the DeepSeek Harness runtime.
//!
//! It owns one job: keep the bundled Node + `dsh web` alive, report its state
//! over NDJSON, and guarantee the whole process tree dies when asked to — or
//! when the parent (Tauri shell / test driver) disappears.
//!
//! Protocol — commands on stdin, events on stdout, one JSON object per line:
//!
//! ```text
//! → {"id":1,"command":"start","node":"…/node","script":"…/lib/bin.js",
//!    "args":["web","--host","127.0.0.1","--port","0"],
//!    "cwd":"…/harness","env":{"DSH_HOME":"…"}}
//! ← {"type":"ack","id":1,"ok":true}
//! ← {"type":"starting"}
//! ← {"type":"log","stream":"stdout","line":"…"}
//! ← {"type":"ready","url":"http://127.0.0.1:49321"}
//! → {"id":2,"command":"status"}   →  {"type":"status","id":2,"state":"running","pid":…,"url":"…"}
//! → {"id":3,"command":"restart"}  →  {"type":"stopping"} … {"type":"stopped","code":0} … {"type":"starting"} …
//! → {"id":4,"command":"shutdown"} →  {"type":"stopped","code":0}   (sidecar keeps running)
//! [stdin EOF]                     →  graceful shutdown of the tree, sidecar exits 0
//! ```

mod platform;

use platform::{PlatformChild, SpawnSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(100);
const FORCE_GRACE: Duration = Duration::from_secs(5);
const NO_CONSOLE_GRACE: Duration = Duration::from_secs(2);

/// Set by OS signal handlers: any termination signal becomes a clean tree
/// teardown. Windows is additionally covered by the Job Object
/// (KILL_ON_JOB_CLOSE), so no handlers are needed there.
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Tags output readers to their child, so late lines from a prior process
/// cannot affect the process currently being supervised.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
extern "C" fn on_termination_signal(_sig: libc::c_int) {
    EXIT_REQUESTED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, on_termination_signal as usize);
        libc::signal(libc::SIGINT, on_termination_signal as usize);
        libc::signal(libc::SIGHUP, on_termination_signal as usize);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {}

/// Extract `http://127.0.0.1:<port>` from the official readiness line:
/// `dsh web: http://127.0.0.1:49321` (optionally with a ` (LAN: …)` suffix).
pub fn extract_local_url(line: &str) -> Option<String> {
    const MARKER: &str = "dsh web: http://127.0.0.1:";
    let idx = line.find(MARKER)?;
    let rest = &line[idx + MARKER.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(format!("http://127.0.0.1:{digits}"))
    }
}

/// A command received over stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start {
        node: String,
        script: String,
        args: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    },
    Shutdown,
    Restart,
    Status,
}

pub fn parse_command(line: &str) -> Result<(Option<u64>, Command), String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("invalid JSON: {e}"))?;
    let id = v.get("id").and_then(|i| i.as_u64());
    let name = v
        .get("command")
        .and_then(|c| c.as_str())
        .ok_or_else(|| "missing \"command\"".to_string())?;
    let cmd = match name {
        "start" => {
            let node = v
                .get("node")
                .and_then(|s| s.as_str())
                .ok_or_else(|| "start: missing \"node\"".to_string())?
                .to_string();
            let script = v
                .get("script")
                .and_then(|s| s.as_str())
                .ok_or_else(|| "start: missing \"script\"".to_string())?
                .to_string();
            let args = v
                .get("args")
                .and_then(|a| a.as_array())
                .ok_or_else(|| "start: \"args\" must be an array".to_string())?
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "start: args must be strings".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let cwd = v
                .get("cwd")
                .and_then(|s| s.as_str())
                .ok_or_else(|| "start: missing \"cwd\"".to_string())?
                .to_string();
            let env = v
                .get("env")
                .and_then(|e| e.as_object())
                .map(|o| {
                    o.iter()
                        .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default();
            Command::Start {
                node,
                script,
                args,
                cwd,
                env,
            }
        }
        "shutdown" => Command::Shutdown,
        "restart" => Command::Restart,
        "status" => Command::Status,
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok((id, cmd))
}

fn emit(obj: Value) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{obj}");
    let _ = lock.flush();
}

fn env_ms(name: &str, default: u64) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(default))
}

fn ack_ok(id: Option<u64>) {
    emit(json!({"type":"ack","id":id,"ok":true}));
}

fn ack_err(id: Option<u64>, error: impl AsRef<str>) {
    emit(json!({"type":"ack","id":id,"ok":false,"error":error.as_ref()}));
}

/// Why we are currently tearing the tree down; decides the exit event.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StopReason {
    Shutdown,
    Restart,
    ReadinessTimeout,
}

/// The child currently being supervised.
struct Running {
    child: PlatformChild,
    pid: u32,
    gen: u64,
}

impl Running {
    fn spawn(spec: &SpawnSpec, tx: &Sender<(u64, &'static str, String)>) -> Result<Self, String> {
        let gen = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        let mut child =
            PlatformChild::spawn(spec).map_err(|e| format!("failed to spawn node: {e}"))?;
        let pid = child.child.id();
        for (stream, pipe) in [
            (
                "stdout",
                child
                    .child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            ),
            (
                "stderr",
                child
                    .child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            ),
        ] {
            if let Some(pipe) = pipe {
                let tx = tx.clone();
                thread::spawn(move || {
                    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                        if !line.is_empty() && tx.send((gen, stream, line)).is_err() {
                            break;
                        }
                    }
                });
            }
        }
        Ok(Running { child, pid, gen })
    }

    fn try_exit(&mut self) -> Option<std::process::ExitStatus> {
        self.child.child.try_wait().ok().flatten()
    }
}

fn main() {
    install_signal_handlers();
    let ready_timeout = env_ms("DSH_READY_TIMEOUT_MS", 120_000);
    let shutdown_grace = env_ms("DSH_SHUTDOWN_GRACE_MS", 10_000);

    emit(json!({"type":"sidecar","version":env!("CARGO_PKG_VERSION")}));

    let (line_tx, line_rx): (
        Sender<(u64, &'static str, String)>,
        Receiver<(u64, &'static str, String)>,
    ) = channel();
    let (cmd_tx, cmd_rx): (Sender<String>, Receiver<String>) = channel();

    // stdin reader: commands in, parent-death detection out (EOF).
    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if cmd_tx.send(line).is_err() {
                break;
            }
        }
        let _ = cmd_tx.send(String::new()); // empty == EOF marker
    });

    let mut running: Option<Running> = None;
    let mut current_gen = 0;
    let mut state = "stopped";
    let mut url: Option<String> = None;
    let mut last_spec: Option<SpawnSpec> = None;

    // Phase deadlines (Some only while relevant).
    let mut ready_deadline: Option<Instant> = None;
    let mut stop_reason: Option<StopReason> = None;
    let mut grace_deadline: Option<Instant> = None;
    let mut forced = false;

    macro_rules! spawn_spec {
        ($spec:expr) => {{
            match Running::spawn(&$spec, &line_tx) {
                Ok(r) => {
                    current_gen = r.gen;
                    running = Some(r);
                    true
                }
                Err(e) => {
                    state = "crashed";
                    emit(json!({"type":"error","code":"spawn-failed","message":e}));
                    false
                }
            }
        }};
    }

    macro_rules! begin_stop {
        ($reason:expr) => {{
            if let Some(r) = running.as_ref() {
                let grace = if r.child.graceful() {
                    shutdown_grace
                } else {
                    emit(json!({"type":"log","stream":"sidecar","line":"graceful stop unavailable (no console); forcing shortly"}));
                    NO_CONSOLE_GRACE
                };
                state = "stopping";
                stop_reason = Some($reason);
                grace_deadline = Some(Instant::now() + grace);
                forced = false;
                emit(json!({"type":"stopping"}));
            } else {
                state = "stopped";
                url = None;
                emit(json!({"type":"stopped","code":null,"pid":null}));
            }
        }};
    }

    loop {
        // 0. Termination signal received: tear the tree down and exit.
        if EXIT_REQUESTED.load(Ordering::SeqCst) {
            if let Some(r) = running.as_ref() {
                r.child.force();
                let deadline = Instant::now() + shutdown_grace;
                while let Some(r) = running.as_mut() {
                    if r.try_exit().is_some() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let _ = r.child.child.kill();
                        let _ = r.try_exit();
                        break;
                    }
                    thread::sleep(TICK);
                }
            }
            std::process::exit(0);
        }

        // 1. Reap the child if it exited.
        let mut exited: Option<(u32, Option<i32>)> = None;
        if let Some(r) = running.as_mut() {
            if let Some(status) = r.try_exit() {
                exited = Some((r.pid, status.code()));
            }
        }
        if let Some((pid, code)) = exited {
            let reason = stop_reason.take();
            running = None;
            ready_deadline = None;
            grace_deadline = None;
            match reason {
                Some(StopReason::Restart) => {
                    emit(json!({"type":"stopped","code":code,"pid":pid}));
                    if let Some(spec) = last_spec.clone() {
                        if spawn_spec!(spec) {
                            state = "starting";
                            url = None;
                            stop_reason = None;
                            grace_deadline = None;
                            forced = false;
                            ready_deadline = Some(Instant::now() + ready_timeout);
                            emit(json!({"type":"starting"}));
                        }
                    } else {
                        state = "stopped";
                        url = None;
                    }
                }
                Some(StopReason::Shutdown) => {
                    emit(json!({"type":"stopped","code":code,"pid":pid}));
                    state = "stopped";
                    url = None;
                }
                None => {
                    emit(json!({"type":"crashed","code":code,"pid":pid}));
                    state = "crashed";
                    url = None;
                }
                Some(StopReason::ReadinessTimeout) => {
                    emit(
                        json!({"type":"crashed","code":code,"pid":pid,"message":"killed after readiness timeout"}),
                    );
                    state = "crashed";
                    url = None;
                }
            }
        }

        // 2. Deadline handling (readiness / graceful→force escalation).
        if running.is_some() {
            if stop_reason.is_none() {
                if let (Some(deadline), true) = (ready_deadline, state == "starting") {
                    if Instant::now() >= deadline {
                        emit(
                            json!({"type":"error","code":"readiness-timeout","message":"dsh web did not print its readiness line in time; killing the tree"}),
                        );
                        begin_stop!(StopReason::ReadinessTimeout);
                    }
                }
            } else if let Some(deadline) = grace_deadline {
                if Instant::now() >= deadline {
                    if !forced {
                        if let Some(r) = running.as_ref() {
                            r.child.force();
                        }
                        forced = true;
                        grace_deadline = Some(Instant::now() + FORCE_GRACE);
                        emit(
                            json!({"type":"log","stream":"sidecar","line":"graceful shutdown timed out; sent force kill"}),
                        );
                    } else if let Some(r) = running.as_mut() {
                        // Last resort: std kill on the direct child.
                        let _ = r.child.child.kill();
                        grace_deadline = Some(Instant::now() + FORCE_GRACE);
                    }
                }
            }
        }

        // 3. Pump child output lines.
        match line_rx.recv_timeout(TICK) {
            Ok((gen, stream, line)) if gen == current_gen => {
                if let Some(u) = extract_local_url(&line) {
                    if state == "starting" {
                        url = Some(u.clone());
                        state = "running";
                        ready_deadline = None;
                        emit(json!({"type":"ready","url":u}));
                    }
                }
                emit(json!({"type":"log","stream":stream,"line":line}));
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Pipes closed while the child is still un-reaped: nothing to read.
                thread::sleep(TICK);
            }
        }

        // 4. Pump commands.
        match cmd_rx.try_recv() {
            Ok(line) if line.is_empty() => {
                // Parent went away: tear the tree down and exit.
                if let Some(r) = running.as_ref() {
                    r.child.force();
                    let deadline = Instant::now() + shutdown_grace;
                    while let Some(r) = running.as_mut() {
                        if r.try_exit().is_some() {
                            break;
                        }
                        if Instant::now() >= deadline {
                            let _ = r.child.child.kill();
                            let _ = r.try_exit();
                            break;
                        }
                        thread::sleep(TICK);
                    }
                }
                std::process::exit(0);
            }
            Ok(line) => match parse_command(&line) {
                Ok((
                    id,
                    Command::Start {
                        node,
                        script,
                        args,
                        cwd,
                        env,
                    },
                )) => {
                    if running.is_some() {
                        ack_err(id, "already started");
                    } else {
                        let spec = SpawnSpec {
                            node,
                            script,
                            args,
                            cwd,
                            env,
                        };
                        if spawn_spec!(spec) {
                            last_spec = Some(spec);
                            ack_ok(id);
                            emit(json!({"type":"starting"}));
                            state = "starting";
                            url = None;
                            stop_reason = None;
                            grace_deadline = None;
                            forced = false;
                            ready_deadline = Some(Instant::now() + ready_timeout);
                        } else {
                            ack_err(id, "spawn failed; see error event");
                        }
                    }
                }
                Ok((id, Command::Shutdown)) => {
                    begin_stop!(StopReason::Shutdown);
                    ack_ok(id);
                }
                Ok((id, Command::Restart)) => {
                    if running.is_some() {
                        begin_stop!(StopReason::Restart);
                        ack_ok(id);
                    } else if let Some(spec) = last_spec.clone() {
                        if spawn_spec!(spec) {
                            last_spec = Some(spec);
                            state = "starting";
                            url = None;
                            stop_reason = None;
                            grace_deadline = None;
                            forced = false;
                            ready_deadline = Some(Instant::now() + ready_timeout);
                            ack_ok(id);
                            emit(json!({"type":"starting"}));
                        } else {
                            ack_err(id, "spawn failed; see error event");
                        }
                    } else {
                        ack_err(id, "nothing to restart");
                    }
                }
                Ok((id, Command::Status)) => {
                    let pid = running.as_ref().map(|r| r.pid);
                    emit(json!({"type":"status","id":id,"state":state,"pid":pid,"url":url}));
                }
                Err(e) => {
                    let id = serde_json::from_str::<Value>(&line)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_u64()));
                    ack_err(id, e);
                }
            },
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                // stdin thread gone without EOF: treat as parent gone.
                if let Some(r) = running.as_ref() {
                    r.child.force();
                }
                std::process::exit(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_readiness_line() {
        assert_eq!(
            extract_local_url("dsh web: http://127.0.0.1:49321"),
            Some("http://127.0.0.1:49321".to_string())
        );
    }

    #[test]
    fn extracts_readiness_with_lan_suffix() {
        assert_eq!(
            extract_local_url("dsh web: http://127.0.0.1:49321 (LAN: http://192.168.1.5:49321)"),
            Some("http://127.0.0.1:49321".to_string())
        );
    }

    #[test]
    fn extracts_prefixed_readiness_line() {
        assert_eq!(
            extract_local_url("2025-08-14 INFO dsh web: http://127.0.0.1:1"),
            Some("http://127.0.0.1:1".to_string())
        );
    }

    #[test]
    fn rejects_unrelated_lines() {
        assert_eq!(extract_local_url("dsh web: listening"), None);
        assert_eq!(extract_local_url("http://127.0.0.1:80"), None);
        assert_eq!(extract_local_url(""), None);
    }

    #[test]
    fn parses_start_command() {
        let (id, cmd) = parse_command(
            r#"{"id":7,"command":"start","node":"/n/node","script":"/s/bin.js","args":["web","--port","0"],"cwd":"/c","env":{"DSH_HOME":"/h"}}"#,
        )
        .unwrap();
        assert_eq!(id, Some(7));
        assert_eq!(
            cmd,
            Command::Start {
                node: "/n/node".into(),
                script: "/s/bin.js".into(),
                args: vec!["web".into(), "--port".into(), "0".into()],
                cwd: "/c".into(),
                env: vec![("DSH_HOME".into(), "/h".into())],
            }
        );
    }

    #[test]
    fn parses_simple_commands() {
        assert_eq!(
            parse_command(r#"{"command":"shutdown"}"#).unwrap(),
            (None, Command::Shutdown)
        );
        assert_eq!(
            parse_command(r#"{"command":"restart"}"#).unwrap(),
            (None, Command::Restart)
        );
        assert_eq!(
            parse_command(r#"{"command":"status"}"#).unwrap(),
            (None, Command::Status)
        );
    }

    #[test]
    fn rejects_bad_commands() {
        assert!(parse_command("not json").is_err());
        assert!(parse_command(r#"{"command":"fly"}"#).is_err());
        assert!(parse_command(r#"{"command":"start","node":"n"}"#).is_err());
    }
}
