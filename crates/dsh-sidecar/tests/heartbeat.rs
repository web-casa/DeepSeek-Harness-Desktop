//! Process-level heartbeat test: drive the REAL sidecar binary with a fake
//! harness that prints the readiness line but never serves HTTP, and assert
//! the full unresponsive chain (error → graceful kill → crashed+message) plus
//! the "supervisor never auto-respawns" contract.
//!
//! Unix-only: the fake harness is `sh -c`. The cross-platform equivalent
//! (bundled Node with a listening-but-silent fake) runs in CI via
//! scripts/verify-heartbeat.ts on all three platforms.

#![cfg(unix)]
// Test-only code: the project's no-unwrap/expect/panic rule exempts tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

struct Sidecar {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    events: Receiver<Value>,
}

impl Sidecar {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dsh-sidecar"))
            .env("DSH_HEARTBEAT_INTERVAL_MS", "200")
            .env("DSH_HEARTBEAT_FAIL_LIMIT", "2")
            .env("DSH_HEARTBEAT_READ_TIMEOUT_MS", "200")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn sidecar");
        let stdout = child.stdout.take().expect("stdout pipe");
        let stdin = child.stdin.take().expect("stdin pipe");
        let stdin = Some(stdin);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        Sidecar {
            child,
            stdin,
            events: rx,
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin.as_mut().unwrap(), "{line}").unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
    }

    fn expect_within(&self, deadline: Instant, pred: impl Fn(&Value) -> bool, what: &str) -> Value {
        loop {
            match self.events.recv_timeout(Duration::from_millis(200)) {
                Ok(v) if pred(&v) => return v,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    assert!(Instant::now() < deadline, "timeout waiting for {what}");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("sidecar stdout closed while waiting for {what}");
                }
            }
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // EOF on stdin is the "parent disappeared" signal: the sidecar must
        // exit 0 with no tree left behind.
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

#[test]
fn unresponsive_child_is_killed_and_reported_as_crashed() {
    let mut sc = Sidecar::spawn();
    // Fake harness: prints the readiness line, then sleeps. The port has no
    // listener, so every probe fails fast → the unresponsive chain fires.
    sc.send(
        r#"{"id":1,"command":"start","node":"sh","script":"-c","args":["echo 'dsh web: http://127.0.0.1:45999'; sleep 30"],"cwd":"/","env":{"DSH_HOME":"/tmp/hb-test-home"}}"#,
    );
    let deadline = Instant::now() + Duration::from_secs(30);

    let ready = sc.expect_within(deadline, |e| e["type"] == "ready", "ready");
    assert!(
        ready["url"].as_str().unwrap().contains("45999"),
        "unexpected ready url: {ready}"
    );

    let err = sc.expect_within(
        deadline,
        |e| e["type"] == "error" && e["code"] == "unresponsive",
        "unresponsive error",
    );
    assert!(
        err["message"].as_str().unwrap().contains("health probes"),
        "unexpected error payload: {err}"
    );

    let crashed = sc.expect_within(deadline, |e| e["type"] == "crashed", "crashed");
    assert_eq!(
        crashed["message"].as_str(),
        Some("killed after health checks failed (unresponsive)")
    );
    assert!(
        crashed["pid"].is_number(),
        "crashed must carry a pid: {crashed}"
    );

    // The supervisor must NOT respawn by itself: after a grace interval there
    // must be no further "ready" event (the shell owns the restart policy).
    std::thread::sleep(Duration::from_millis(600));
    while let Ok(v) = sc.events.try_recv() {
        assert_ne!(v["type"], "ready", "sidecar must not auto-respawn: {v}");
    }
}
