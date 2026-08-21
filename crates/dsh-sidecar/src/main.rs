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
//!    "args":["web","--no-open","--host","127.0.0.1","--port","0"],
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
//!
//! While running, the sidecar also probes the ready URL (liveness heartbeat);
//! after `DSH_HEARTBEAT_FAIL_LIMIT` consecutive unanswered probes it declares
//! the child hung and emits the same pair the readiness watchdog does:
//!
//! ```text
//! ← {"type":"error","code":"unresponsive","message":"dsh web did not answer health probes; killing the tree"}
//! ← {"type":"crashed","code":null,"pid":…,"message":"killed after health checks failed (unresponsive)"}
//! ```
//!
//! `crashed` events MAY carry a `message`; the shell must prefer it over the
//! generic exit-code wording. The shell owns the restart policy for every
//! `crashed`, unresponsive included.

use dsh_sidecar::platform::{self, PlatformChild, SpawnSpec};
use serde_json::{json, Value};
use std::io::{BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(100);
const FORCE_GRACE: Duration = Duration::from_secs(5);
const MAX_LINE: usize = 8192;
const MAX_COMMAND_LINE: usize = 64 * 1024;
/// Bound child-output memory while still allowing short bursts. Readers block
/// on this queue once full, propagating backpressure into the OS pipe.
const LINE_CHANNEL_CAPACITY: usize = 256;
/// Yield to heartbeat/command handling after each output burst.
const MAX_LINE_BATCH: usize = 64;

/// Pure truncation oracle retained for the property tests. Production output
/// uses `for_each_bounded_line`, which applies the cap while reading rather
/// than after an unbounded `String` has already been allocated.
#[cfg(test)]
fn truncate_line(line: String) -> String {
    if line.len() <= MAX_LINE {
        return line;
    }
    // Find the largest character boundary at or below MAX_LINE bytes.
    let mut cut = MAX_LINE;
    while cut > 0 && !line.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = String::with_capacity(cut + 24);
    out.push_str(&line[..cut]);
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

fn env_count(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Heartbeat durations: 0 keeps its documented "disabled" meaning; any other
/// value below the floor is raised, so a misconfigured knob (e.g. 1ms
/// interval + connection-refused probes) cannot turn the watcher into a
/// busy loop.
const MIN_HB_STEP: Duration = Duration::from_millis(100);
fn clamp_heartbeat(d: Duration) -> Duration {
    if d.is_zero() || d >= MIN_HB_STEP {
        d
    } else {
        MIN_HB_STEP
    }
}

// ---------------------------------------------------------------------------
// Liveness heartbeat: a child can die two ways — the process exits (reaped
// above → `crashed`) or it stays alive with a blocked event loop, answering
// nothing. The latter would leave the UI silently white forever, so after
// `ready` we probe the web endpoint and declare the child hung after N
// consecutive unanswered probes. The supervisor itself never restarts: it
// emits `crashed` and the Tauri shell owns the restart policy (backoff +
// attempt cap), exactly like a real crash.
// ---------------------------------------------------------------------------

/// Consecutive-failure counter; the pure decision core of the heartbeat.
struct Heartbeat {
    consecutive: u32,
    limit: u32,
}

impl Heartbeat {
    fn new(limit: u32) -> Self {
        Heartbeat {
            consecutive: 0,
            limit,
        }
    }

    /// Record one probe outcome. Returns true when the child is declared hung
    /// (the caller should kill the tree and emit `crashed`).
    fn observe(&mut self, ok: bool) -> bool {
        if ok {
            self.consecutive = 0;
            false
        } else {
            self.consecutive += 1;
            self.consecutive >= self.limit
        }
    }
}

/// Minimal HTTP GET probe against the ready URL (`http://127.0.0.1:<port>` by
/// construction — `extract_local_url` only accepts that shape). Alive = any
/// response bytes within the read timeout; connection refused, write failure,
/// read timeout, and immediate EOF all count as dead. A Node process whose
/// event loop is blocked never writes a response, even though the kernel
/// still completes the TCP handshake — so "listening but silent" is exactly
/// what the read timeout catches.
fn http_probe(url: &str, read_timeout: Duration) -> bool {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    let Some(port) = url.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) else {
        return false;
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(read_timeout));
    let Ok(_) = stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
    else {
        return false;
    };
    let mut buf = [0u8; 64];
    stream.read(&mut buf).is_ok_and(|n| n > 0)
}

/// Why we are currently tearing the tree down; decides the exit event.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StopReason {
    Shutdown,
    Restart,
    ReadinessTimeout,
    Unresponsive,
}

/// The child currently being supervised.
struct Running {
    child: PlatformChild,
    pid: u32,
    gen: u64,
}

impl Running {
    fn spawn(spec: &SpawnSpec, tx: &SyncSender<LineEvent>) -> Result<Self, String> {
        let gen = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        // OsString snapshot: `std::env::vars()` would panic on non-UTF-8 env.
        // RAW on purpose — the sanitizer lives inside PlatformChild::spawn.
        let inherited = std::env::vars_os().collect::<Vec<_>>();
        let mut child = PlatformChild::spawn(spec, &inherited)
            .map_err(|e| format!("failed to spawn node: {e}"))?;
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
                    let _ = dsh_sidecar::for_each_bounded_line(
                        BufReader::new(pipe),
                        MAX_LINE,
                        |line| {
                            if line.is_empty() {
                                return true;
                            }
                            tx.send((gen, stream, line)).is_ok()
                        },
                    );
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
    // Heartbeat knobs: interval 0 disables the watcher entirely; FAIL_LIMIT
    // must be >= 1 (0/illegal coerces to the default). Defaults are
    // deliberately conservative (4 unanswered probes × 10s ≈ 40s of silence)
    // so long synchronous work cannot be mistaken for a hang. Non-zero
    // durations below 100ms are clamped up (busy-loop guard).
    let hb_interval = clamp_heartbeat(env_ms("DSH_HEARTBEAT_INTERVAL_MS", 10_000));
    let hb_read_timeout = clamp_heartbeat(env_ms("DSH_HEARTBEAT_READ_TIMEOUT_MS", 3_000));
    let hb_fail_limit = env_count("DSH_HEARTBEAT_FAIL_LIMIT", 4);

    emit(json!({"type":"sidecar","version":env!("CARGO_PKG_VERSION")}));

    let (line_tx, line_rx): (SyncSender<LineEvent>, Receiver<LineEvent>) =
        sync_channel(LINE_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx): (Sender<String>, Receiver<String>) = channel();
    // Hang reports from heartbeat watchers; the payload is the generation of
    // the child the watcher was probing, so a stale watcher's verdict can
    // never kill a newer child.
    let (hb_tx, hb_rx): (Sender<u64>, Receiver<u64>) = channel();

    // Watcher control: hb_gen tracks the currently supervised generation,
    // hb_enabled is cleared whenever we are not (or stop being) "running".
    let hb_gen: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let hb_enabled: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // stdin reader: commands in. Parent death is detected via channel
    // disconnect below (the thread ends on stdin EOF, closing the sender).
    thread::spawn(move || {
        let stdin = std::io::stdin();
        let _ = dsh_sidecar::for_each_bounded_line(
            BufReader::new(stdin.lock()),
            MAX_COMMAND_LINE,
            |line| cmd_tx.send(line).is_ok(),
        );
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
                    hb_gen.store(r.gen, Ordering::Relaxed);
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
            hb_enabled.store(false, Ordering::Relaxed);
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

    macro_rules! handle_line_event {
        ($event:expr) => {{
            let (gen, stream, line) = $event;
            if gen == current_gen {
                if let Some(u) = extract_local_url(&line) {
                    if state == "starting" {
                        url = Some(u.clone());
                        state = "running";
                        ready_deadline = None;
                        emit(json!({"type":"ready","url":u}));
                        // Arm the liveness watcher for THIS generation. It
                        // exits on the first of: disabled (stop/shutdown),
                        // generation change (restart/respawn), or a hang
                        // verdict (reported via hb_tx with its generation).
                        if !hb_interval.is_zero() {
                            hb_enabled.store(true, Ordering::Relaxed);
                            let my_gen = current_gen;
                            let url_c = u.clone();
                            let gen_c = Arc::clone(&hb_gen);
                            let enabled_c = Arc::clone(&hb_enabled);
                            let tx = hb_tx.clone();
                            let interval = hb_interval;
                            let read_timeout = hb_read_timeout;
                            let limit = hb_fail_limit;
                            thread::spawn(move || {
                                let mut hb = Heartbeat::new(limit);
                                loop {
                                    thread::sleep(interval);
                                    if !enabled_c.load(Ordering::Relaxed)
                                        || gen_c.load(Ordering::Relaxed) != my_gen
                                    {
                                        return;
                                    }
                                    if hb.observe(http_probe(&url_c, read_timeout)) {
                                        let _ = tx.send(my_gen);
                                        return;
                                    }
                                }
                            });
                        }
                    }
                }
                emit(json!({"type":"log","stream":stream,"line":line}));
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
            hb_enabled.store(false, Ordering::Relaxed);
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
                Some(StopReason::Unresponsive) => {
                    emit(
                        json!({"type":"crashed","code":code,"pid":pid,"message":"killed after health checks failed (unresponsive)"}),
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
            Ok(event) => {
                handle_line_event!(event);
                for _ in 1..MAX_LINE_BATCH {
                    match line_rx.try_recv() {
                        Ok(event) => handle_line_event!(event),
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Pipes closed while the child is still un-reaped: nothing to read.
                thread::sleep(TICK);
            }
        }

        // 4. Pump heartbeat hang reports.
        match hb_rx.try_recv() {
            Ok(gen) => {
                // Only a verdict for the CURRENTLY running generation may act;
                // a stale watcher's report is ignored (the child it probed is
                // already gone).
                let is_current = running.as_ref().is_some_and(|r| r.gen == gen);
                if is_current && state == "running" && stop_reason.is_none() {
                    emit(
                        json!({"type":"error","code":"unresponsive","message":"dsh web did not answer health probes; killing the tree"}),
                    );
                    begin_stop!(StopReason::Unresponsive);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {}
        }

        // 5. Pump commands.
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
    use dsh_sidecar::{quote_arg, sanitize_env_lines, sanitize_inherited_env};

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
        use dsh_sidecar::quote_arg;
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
            // Byte-semantics bounds: the kept prefix must be a source prefix
            // cut at the LARGEST char boundary ≤ MAX_LINE, with the marker
            // appended. (An earlier char-count-based assertion passed only
            // because `any::<String>()` rarely exceeds the byte cap.)
            fn truncate_line_bounds(s in any::<String>()) {
                const MARKER: &str = "… [line truncated]";
                let out = truncate_line(s.clone());
                if s.len() <= MAX_LINE {
                    prop_assert_eq!(out, s);
                } else {
                    let kept = out.strip_suffix(MARKER).expect("marker required past the cap");
                    prop_assert!(kept.len() <= MAX_LINE);
                    prop_assert!(kept.is_char_boundary(kept.len()));
                    prop_assert!(s.starts_with(kept));
                    let next_char = s[kept.len()..].chars().next().expect("truncation cut content");
                    prop_assert!(
                        kept.len() + next_char.len_utf8() > MAX_LINE,
                        "cut must be the largest char boundary at or below MAX_LINE"
                    );
                }
            }

            // Long lines made only of multi-byte chars: byte 8192 lands
            // mid-character for most n — regression for the byte-slice panic.
            // ("中" = 3 bytes, "🙂" = 4 bytes → 7-byte units; 8192 % 7 = 2.)
            #[test]
            fn truncate_line_multibyte_heavy_never_panics(n in 1200usize..6000usize) {
                let line = "中🙂".repeat(n);
                prop_assert!(line.len() > MAX_LINE, "setup: must exceed the cap");
                let out = truncate_line(line);
                let prefix = out
                    .strip_suffix("… [line truncated]")
                    .expect("marker required past the cap");
                prop_assert!(prefix.len() <= MAX_LINE);
                prop_assert!(prefix.is_char_boundary(prefix.len()));
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
        use std::io::{BufRead, BufReader, Write};
        use std::process::{Command as ProcessCommand, Stdio};
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
            let mut child = PlatformChild::spawn(&spec("exit 7"), &proc_env()).unwrap();
            let status =
                wait_exit(&mut child, Duration::from_secs(10)).expect("child did not exit");
            assert_eq!(status.code(), Some(7));
        }

        #[test]
        fn graceful_then_force_kills_a_trapping_child() {
            // Trap TERM: the graceful signal is delivered but ignored, so the
            // force path (SIGKILL to the group) must finish the job.
            let mut child =
                PlatformChild::spawn(&spec("trap '' TERM; sleep 30"), &proc_env()).unwrap();
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
            let mut child = PlatformChild::spawn(&spec("sleep 30"), &proc_env()).unwrap();
            assert!(child.graceful());
            let status = wait_exit(&mut child, Duration::from_secs(10)).expect("TERM did not stop");
            assert!(!status.success());
        }

        #[test]
        fn env_overrides_reach_the_child() {
            let mut spec = spec("test \"$DSH_HOME\" = \"/tmp/qa-home\"");
            spec.env = vec![("DSH_HOME".to_string(), "/tmp/qa-home".to_string())];
            let mut child = PlatformChild::spawn(&spec, &proc_env()).unwrap();
            let status =
                wait_exit(&mut child, Duration::from_secs(10)).expect("child did not exit");
            assert_eq!(status.code(), Some(0));
        }

        /// The real process env snapshot, for tests that only need a working
        /// PATH (sleep/test are external binaries). The injection test below
        /// uses a crafted snapshot instead — no process-global set_var.
        fn proc_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
            std::env::vars_os().collect()
        }

        #[test]
        fn inherited_node_env_is_sanitized_for_the_child() {
            // The injection chain this guards against: user shell env →
            // sidecar → bundled Node. Feed a crafted inherited snapshot (no
            // process-global mutation) and assert the child sees the poison
            // removed while ordinary keys (PATH) still flow through.
            let inherited = vec![
                ("NODE_OPTIONS".into(), "--require=/evil.js".into()),
                ("npm_config_cache".into(), "/tmp/evil-cache".into()),
                (
                    "PATH".into(),
                    std::env::var_os("PATH").expect("PATH set in test env"),
                ),
            ];
            let child = PlatformChild::spawn(
                &spec(
                    "test -z \"${NODE_OPTIONS}\" && test -z \"${npm_config_cache}\" && test -n \"${PATH}\"",
                ),
                &inherited,
            );
            let mut child = child.unwrap();
            let status =
                wait_exit(&mut child, Duration::from_secs(10)).expect("child did not exit");
            assert_eq!(status.code(), Some(0));
        }

        /// Re-exec helper for `abrupt_parent_death_kills_the_process_tree`.
        /// It intentionally remains alive until the outer test sends SIGKILL,
        /// proving cleanup does not depend on Drop or a signal handler.
        #[test]
        fn abrupt_parent_death_helper() {
            if std::env::var_os("DSH_PARENT_DEATH_HELPER").is_none() {
                return;
            }
            let mut child = PlatformChild::spawn(
                &spec("sleep 60 & echo DSH_TREE_PIDS=$$,$!; wait"),
                &proc_env(),
            )
            .unwrap();
            let stdout = child.child.stdout.take().expect("child stdout");
            let mut reader = BufReader::new(stdout);
            let mut marker = String::new();
            reader.read_line(&mut marker).expect("read child pids");
            print!("{marker}");
            std::io::stdout().flush().expect("flush child pids");
            std::thread::sleep(Duration::from_secs(60));
        }

        #[test]
        fn abrupt_parent_death_kills_the_process_tree() {
            const HELPER: &str = "tests::platform_tests::abrupt_parent_death_helper";
            let mut helper = ProcessCommand::new(std::env::current_exe().unwrap())
                .args(["--exact", HELPER, "--nocapture"])
                .env("DSH_PARENT_DEATH_HELPER", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let stdout = helper.stdout.take().expect("helper stdout");
            let (marker_tx, marker_rx) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(raw) = line.strip_prefix("DSH_TREE_PIDS=") {
                        let pids = raw.split_once(',').and_then(|(leader, descendant)| {
                            Some((leader.parse::<i32>().ok()?, descendant.parse::<i32>().ok()?))
                        });
                        if let Some(pids) = pids {
                            let _ = marker_tx.send(pids);
                        }
                        return;
                    }
                }
            });
            let pids = marker_rx.recv_timeout(Duration::from_secs(10)).ok();
            if pids.is_none() {
                let _ = helper.kill();
                let _ = helper.wait();
            }
            let (leader, descendant) = pids.expect("helper did not report child pids");

            unsafe {
                libc::kill(helper.id() as i32, libc::SIGKILL);
            }
            let _ = helper.wait();

            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline
                && (process_is_live(leader) || process_is_live(descendant))
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            let leader_live = process_is_live(leader);
            let descendant_live = process_is_live(descendant);
            if leader_live || descendant_live {
                unsafe {
                    libc::kill(leader, libc::SIGKILL);
                    libc::kill(descendant, libc::SIGKILL);
                }
            }
            assert!(!leader_live, "process-group leader survived parent SIGKILL");
            assert!(
                !descendant_live,
                "Harness descendant survived parent SIGKILL"
            );
        }

        fn process_is_live(pid: i32) -> bool {
            let output = ProcessCommand::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output();
            match output {
                Ok(output) if output.status.success() => {
                    let state = String::from_utf8_lossy(&output.stdout);
                    let state = state.trim();
                    !state.is_empty() && !state.starts_with('Z')
                }
                _ => unsafe {
                    libc::kill(pid, 0) == 0
                        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                },
            }
        }
    }

    #[test]
    fn sanitizes_inherited_env_strings() {
        use std::ffi::OsString;
        let vars: Vec<(OsString, OsString)> = vec![
            ("PATH".into(), "/usr/bin".into()),
            ("NODE_OPTIONS".into(), "--require=/evil".into()),
            ("node_path".into(), "/evil".into()),
            ("npm_config_cache".into(), "/c".into()),
            ("Npm_Config_Foo".into(), "1".into()),
            ("ELECTRON_RUN_AS_NODE".into(), "1".into()),
            ("HOME".into(), "/home/u".into()),
        ];
        let out = sanitize_inherited_env(vars);
        let keys: Vec<&str> = out
            .iter()
            .map(|(k, _)| k.to_str().expect("ASCII keys"))
            .collect();
        assert_eq!(keys, vec!["PATH", "HOME"]);
    }

    #[cfg(unix)]
    #[test]
    fn sanitize_never_panics_on_non_utf8_env() {
        use std::ffi::{OsStr, OsString};
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        // A raw byte sequence in a VALUE (e.g. a filename) — std::env::vars()
        // panics on exactly this; the OsString path must pass it through.
        let weird_value = OsString::from_vec(vec![0xFF, 0xFE, b'x']);
        let weird_key = OsStr::from_bytes(&[0xFF, b'=']);
        let vars = vec![
            ("PATH".into(), weird_value),
            (weird_key.to_os_string(), "v".into()),
            ("NODE_OPTIONS".into(), "--require=/evil".into()),
        ];
        let out = sanitize_inherited_env(vars);
        assert_eq!(out.len(), 2);
        // Non-UTF-8 value survives byte-for-byte; forbidden ASCII key dropped.
        assert_eq!(out[0].0, OsStr::new("PATH"));
        assert_eq!(out[0].1.as_encoded_bytes(), &[0xFF, 0xFE, b'x']);
        assert_eq!(out[1].0, weird_key);
    }

    #[test]
    fn sanitizes_utf16_env_lines_without_roundtrip() {
        fn u(s: &str) -> Vec<u16> {
            s.encode_utf16().collect()
        }
        // A lone high surrogate in an UNRELATED entry must survive verbatim:
        // the sanitizer never round-trips through UTF-8.
        let mut path_line = u("PATH=");
        path_line.push(0xD800);
        let lines = vec![
            u("NODE_OPTIONS=--require=x"),
            u("npm_config_foo=1"),
            u("Node_Path=/evil"),
            u("DYLD_INSERT_LIBRARIES=/evil.dylib"),
            u("LD_PRELOAD=/evil.so"),
            path_line,
            u("=C:=C:\\dir"),
            u("NO_EQUALS"),
            u("HOME=/u"),
        ];
        let out = sanitize_env_lines(lines);
        assert_eq!(out.len(), 4);
        let mut want_path = u("PATH=");
        want_path.push(0xD800);
        assert_eq!(out[0], want_path);
        assert_eq!(out[1], u("=C:=C:\\dir"));
        assert_eq!(out[2], u("NO_EQUALS"));
        assert_eq!(out[3], u("HOME=/u"));
    }

    #[test]
    fn heartbeat_resets_on_success_and_flags_after_limit_failures() {
        let mut hb = Heartbeat::new(3);
        assert!(!hb.observe(false));
        assert!(!hb.observe(false), "two failures must not hang yet");
        assert!(hb.observe(false), "the third consecutive failure hangs");
        // A later success resets the streak.
        assert!(!hb.observe(true));
    }

    #[test]
    fn heartbeat_success_resets_the_failure_streak() {
        let mut hb = Heartbeat::new(3);
        assert!(!hb.observe(false));
        assert!(!hb.observe(false));
        assert!(!hb.observe(true), "success must clear the streak");
        assert!(!hb.observe(false));
        assert!(!hb.observe(false), "still below the limit after the reset");
        assert!(hb.observe(false));
    }

    #[test]
    fn heartbeat_limit_one_hangs_on_first_failure() {
        let mut hb = Heartbeat::new(1);
        assert!(hb.observe(false));
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
        assert_eq!(
            json!({"type":"error","code":"unresponsive","message":"dsh web did not answer health probes; killing the tree"})
                .to_string(),
            r#"{"code":"unresponsive","message":"dsh web did not answer health probes; killing the tree","type":"error"}"#
        );
        // The crashed+message shape is the heartbeat's contract with the
        // shell: it must surface `message`, not just the exit code.
        assert_eq!(
            json!({"type":"crashed","code":null,"pid":42,"message":"killed after health checks failed (unresponsive)"})
                .to_string(),
            r#"{"code":null,"message":"killed after health checks failed (unresponsive)","pid":42,"type":"crashed"}"#
        );
        // The readiness-timeout kill uses the same shape — both must be
        // pinned so a refactor cannot silently change either side.
        assert_eq!(
            json!({"type":"crashed","code":143,"pid":7,"message":"killed after readiness timeout"})
                .to_string(),
            r#"{"code":143,"message":"killed after readiness timeout","pid":7,"type":"crashed"}"#
        );
    }

    #[test]
    fn truncate_line_is_char_boundary_safe() {
        const MARKER: &str = "… [line truncated]";
        // '中' is 3 bytes: 2731 * 3 = 8193, so byte 8192 falls 2 bytes into the
        // final char. The buggy byte-slice version panics on exactly this input.
        let line = "中".repeat(2731);
        assert_eq!(line.len(), MAX_LINE + 1);
        assert!(
            !line.is_char_boundary(MAX_LINE),
            "setup: boundary must be mid-char"
        );
        let out = truncate_line(line.clone());
        let kept = out.strip_suffix(MARKER).expect("truncation marker present");
        assert!(kept.len() <= MAX_LINE);
        assert!(
            line.starts_with(kept),
            "kept prefix must come from the source"
        );
        assert!(kept.is_char_boundary(kept.len()) || kept.is_empty());
        // The cut really landed inside the multi-byte char: 8190 = 2730 * 3.
        assert_eq!(kept, "中".repeat(2730));
    }
}
