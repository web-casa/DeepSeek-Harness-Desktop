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
const MAX_LINE: usize = 8192;

/// Quote one argument for the Windows command line (CommandLineToArgvW
/// semantics). Pure string logic — unit-tested on every platform even though
/// only the Windows spawn path consumes it.
pub fn quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    if !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Cap a single log line: `BufReader::lines` reads unbounded, and whatever we
/// forward is amplified through the supervisor and the Tauri logs ring.
fn truncate_line(line: String) -> String {
    if line.len() <= MAX_LINE {
        return line;
    }
    let mut out = String::with_capacity(MAX_LINE + 24);
    out.push_str(&line[..MAX_LINE]);
    out.push_str("… [line truncated]");
    out
}

/// A child output line tagged with the generation of the child that produced
/// it — stale lines from a previous child are dropped by the main loop.
type LineEvent = (u64, &'static str, String);
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
    // sigaction (not the deprecated libc::signal): SA_RESTART keeps the
    // supervisor loop's syscalls from being interrupted by stray signals.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_termination_signal as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut());
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
        return None;
    }
    // Port must be followed by end of line or the documented " (LAN: …)"
    // suffix — arbitrary trailing text must never count as readiness.
    let tail = &rest[digits.len()..];
    if !tail.is_empty() && !tail.starts_with(" (LAN: ") {
        return None;
    }
    let port: u16 = digits.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some(format!("http://127.0.0.1:{port}"))
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
            let env = match v.get("env") {
                None => Vec::new(),
                Some(e) if e.is_object() => {
                    e.as_object()
                        .map(|o| {
                            o.iter()
                                .map(|(k, val)| {
                                    val.as_str().map(|v| (k.clone(), v.to_string())).ok_or_else(
                                        || format!("start: env value for {k:?} must be a string"),
                                    )
                                })
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()?
                        .unwrap_or_default()
                }
                Some(_) => return Err("start: \"env\" must be an object".to_string()),
            };
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
    fn spawn(spec: &SpawnSpec, tx: &Sender<LineEvent>) -> Result<Self, String> {
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
                        if line.is_empty() {
                            continue;
                        }
                        // Cap amplification: a single unbounded harness line
                        // must not balloon through the supervisor's queues.
                        let line = truncate_line(line);
                        if tx.send((gen, stream, line)).is_err() {
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
    // Windows: a hidden console lets the child inherit one, which is what
    // makes GenerateConsoleCtrlEvent(CTRL_C) reachable for graceful stop.
    platform::ensure_hidden_console();
    let ready_timeout = env_ms("DSH_READY_TIMEOUT_MS", 120_000);
    let shutdown_grace = env_ms("DSH_SHUTDOWN_GRACE_MS", 10_000);

    emit(json!({"type":"sidecar","version":env!("CARGO_PKG_VERSION")}));

    let (line_tx, line_rx): (Sender<LineEvent>, Receiver<LineEvent>) = channel();
    let (cmd_tx, cmd_rx): (Sender<String>, Receiver<String>) = channel();

    // stdin reader: commands in. Parent death is detected via channel
    // disconnect below (the thread ends on stdin EOF, closing the sender).
    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if cmd_tx.send(line).is_err() {
                break;
            }
        }
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
            Ok(line) if line.trim().is_empty() => {
                // Blank lines are ignored — no out-of-band sentinel protocol.
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
        // Malformed ports must be rejected, never silently truncated.
        assert_eq!(extract_local_url("dsh web: http://127.0.0.1:0"), None);
        assert_eq!(extract_local_url("dsh web: http://127.0.0.1:123abc"), None);
        assert_eq!(extract_local_url("dsh web: http://127.0.0.1:70000"), None);
        assert_eq!(extract_local_url("dsh web: http://127.0.0.1:49321x"), None);
        assert_eq!(
            extract_local_url("dsh web: http://127.0.0.1:49321 attacker"),
            None
        );
        assert_eq!(
            extract_local_url("dsh web: http://127.0.0.1:49321 (LAN: http://192.168.1.5:49321)"),
            Some("http://127.0.0.1:49321".to_string())
        );
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

    #[test]
    fn rejects_non_string_env_values() {
        let result = parse_command(
            r#"{"command":"start","node":"n","script":"s","args":[],"cwd":"c","env":{"DSH_HOME":123}}"#,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("must be a string"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_non_object_env() {
        let result = parse_command(
            r#"{"command":"start","node":"n","script":"s","args":[],"cwd":"c","env":[["DSH_HOME","/h"]]}"#,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("must be an object"), "unexpected error: {err}");
    }

    #[test]
    fn quotes_plain_args_verbatim() {
        assert_eq!(quote_arg("node.exe"), "node.exe");
        assert_eq!(quote_arg("--port"), "--port");
    }

    #[test]
    fn quotes_args_with_spaces() {
        assert_eq!(quote_arg("DeepSeek Harness"), "\"DeepSeek Harness\"");
    }

    #[test]
    fn quotes_empty_args() {
        assert_eq!(quote_arg(""), "\"\"");
    }

    #[test]
    fn escapes_embedded_quotes() {
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn handles_backslashes_before_quotes_and_at_end() {
        assert_eq!(quote_arg("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(
            quote_arg("C:\\Program Files\\"),
            "\"C:\\Program Files\\\\\""
        );
    }

    // ------------------------------------------------------------------
    // Property-based tests: the adversarial inputs (framing, quoting,
    // parsing) are exactly what property tests are for.
    // ------------------------------------------------------------------
    mod proptests {
        use crate::*;
        use proptest::prelude::*;

        /// Minimal CommandLineToArgvW-compatible unquoter — the reference
        /// implementation our quote_arg must round-trip against.
        fn unquote_arg(s: &str) -> String {
            if !s.starts_with('"') {
                return s.to_string();
            }
            let mut chars = s[1..].chars().peekable();
            let mut out = String::new();
            let mut backslashes = 0usize;
            let mut quoted = true;
            while let Some(c) = chars.next() {
                if c == '\\' {
                    backslashes += 1;
                    continue;
                }
                if c == '"' {
                    out.push_str(&"\\".repeat(backslashes / 2));
                    if backslashes % 2 == 1 {
                        out.push('"');
                    } else {
                        quoted = false;
                        // After a closing quote, remaining chars are literal;
                        // our quote_arg never emits them, so just drain.
                        for rest in chars.by_ref() {
                            out.push(rest);
                        }
                    }
                    backslashes = 0;
                    continue;
                }
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
            if quoted {
                out.push_str(&"\\".repeat(backslashes));
            }
            out
        }

        proptest! {
            #[test]
            fn quote_arg_roundtrips(s in any::<String>()) {
                prop_assert_eq!(unquote_arg(&quote_arg(&s)), s);
            }

            #[test]
            fn extract_local_url_never_panics(s in any::<String>()) {
                let _ = extract_local_url(&s);
            }

            #[test]
            fn extract_local_url_parses_any_port(port in 0u32..=99_999) {
                let line = format!("dsh web: http://127.0.0.1:{port}");
                let parsed = extract_local_url(&line);
                let expected = if (1..=65_535).contains(&port) {
                    line.strip_prefix("dsh web: ").map(str::to_string)
                } else {
                    None
                };
                prop_assert_eq!(parsed, expected);
            }

            #[test]
            fn parse_command_never_panics(s in any::<String>()) {
                let _ = parse_command(&s);
            }

            #[test]
            fn parse_start_roundtrips(env in prop::collection::vec(
                any::<String>(), 0..8
            ).prop_map(|vals| vals.into_iter().enumerate().map(|(i, v)| (format!("K{i}"), v)).collect::<Vec<_>>())) {
                let env_map: serde_json::Map<String, Value> = env
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect();
                let cmd = json!({
                    "command": "start",
                    "node": "node",
                    "script": "script",
                    "args": ["web"],
                    "cwd": "cwd",
                    "env": env_map,
                });
                let text = cmd.to_string();
                let (_, parsed) = parse_command(&text).unwrap();
                // Re-serializing the parsed command must yield the same fields.
                match parsed {
                    Command::Start { node, script, args, cwd, env: env2 } => {
                        prop_assert_eq!(node, "node");
                        prop_assert_eq!(script, "script");
                        prop_assert_eq!(args, vec!["web".to_string()]);
                        prop_assert_eq!(cwd, "cwd");
                        prop_assert_eq!(env2, env);
                    }
                    _ => prop_assert!(false, "expected Start"),
                }
            }

            #[test]
            fn truncate_line_bounds(s in any::<String>()) {
                let out = truncate_line(s.clone());
                prop_assert!(out.len() <= MAX_LINE + "… [line truncated]".len() + 8);
                let keep = s.chars().count().min(MAX_LINE);
                prop_assert!(out.starts_with(&s.chars().take(keep).collect::<String>()));
            }
        }
    }

    // ------------------------------------------------------------------
    // Platform integration tests (unix): spawn/graceful/force against a
    // real subprocess — the branches unit tests cannot reach. Windows
    // equivalents are exercised by the CI runtime smoke.
    // ------------------------------------------------------------------
    #[cfg(unix)]
    mod platform_tests {
        use super::*;
        use platform::{PlatformChild, SpawnSpec};
        use std::time::{Duration, Instant};

        fn spec(command: &str) -> SpawnSpec {
            SpawnSpec {
                node: "sh".to_string(),
                script: "-c".to_string(),
                args: vec![command.to_string()],
                cwd: "/".to_string(),
                env: vec![],
            }
        }

        fn wait_exit(
            child: &mut PlatformChild,
            timeout: Duration,
        ) -> Option<std::process::ExitStatus> {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(status) = child.child.try_wait().unwrap() {
                    return Some(status);
                }
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        #[test]
        fn spawn_and_reap_normal_exit() {
            let mut child = PlatformChild::spawn(&spec("exit 7")).unwrap();
            let status =
                wait_exit(&mut child, Duration::from_secs(10)).expect("child did not exit");
            assert_eq!(status.code(), Some(7));
        }

        #[test]
        fn graceful_then_force_kills_a_trapping_child() {
            // Trap TERM: the graceful signal is delivered but ignored, so the
            // force path (SIGKILL to the group) must finish the job.
            let mut child = PlatformChild::spawn(&spec("trap '' TERM; sleep 30")).unwrap();
            // Give the shell time to install the trap before signalling.
            std::thread::sleep(Duration::from_millis(500));
            assert!(child.graceful(), "graceful signal must be deliverable");
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                child.child.try_wait().unwrap().is_none(),
                "child must survive TERM"
            );
            child.force();
            let status =
                wait_exit(&mut child, Duration::from_secs(10)).expect("force did not kill");
            assert!(!status.success());
        }

        #[test]
        fn graceful_stops_a_normal_child() {
            let mut child = PlatformChild::spawn(&spec("sleep 30")).unwrap();
            assert!(child.graceful());
            let status = wait_exit(&mut child, Duration::from_secs(10)).expect("TERM did not stop");
            assert!(!status.success());
        }

        #[test]
        fn env_overrides_reach_the_child() {
            let mut spec = spec("test \"$DSH_HOME\" = \"/tmp/qa-home\"");
            spec.env = vec![("DSH_HOME".to_string(), "/tmp/qa-home".to_string())];
            let mut child = PlatformChild::spawn(&spec).unwrap();
            let status =
                wait_exit(&mut child, Duration::from_secs(10)).expect("child did not exit");
            assert_eq!(status.code(), Some(0));
        }
    }

    #[test]
    fn ndjson_golden_events() {
        // These exact serialized forms are the protocol contract parsed by
        // the Tauri shell and verify-runtime; changing them silently breaks
        // the other side of the pipe.
        assert_eq!(
            json!({"type":"ack","id":7,"ok":true}).to_string(),
            r#"{"id":7,"ok":true,"type":"ack"}"#
        );
        assert_eq!(
            json!({"type":"starting"}).to_string(),
            r#"{"type":"starting"}"#
        );
        assert_eq!(
            json!({"type":"ready","url":"http://127.0.0.1:41234"}).to_string(),
            r#"{"type":"ready","url":"http://127.0.0.1:41234"}"#
        );
        assert_eq!(
            json!({"type":"stopped","code":0,"pid":42}).to_string(),
            r#"{"code":0,"pid":42,"type":"stopped"}"#
        );
        assert_eq!(
            json!({"type":"crashed","code":1,"pid":42}).to_string(),
            r#"{"code":1,"pid":42,"type":"crashed"}"#
        );
        assert_eq!(
            json!({"type":"status","id":9,"state":"running","pid":42,"url":"http://127.0.0.1:1"})
                .to_string(),
            r#"{"id":9,"pid":42,"state":"running","type":"status","url":"http://127.0.0.1:1"}"#
        );
        assert_eq!(
            json!({"type":"log","stream":"stdout","line":"x"}).to_string(),
            r#"{"line":"x","stream":"stdout","type":"log"}"#
        );
        assert_eq!(
            json!({"type":"error","code":"readiness-timeout","message":"m"}).to_string(),
            r#"{"code":"readiness-timeout","message":"m","type":"error"}"#
        );
    }
}
