// Liveness-heartbeat e2e: drive the REAL sidecar against fake harnesses built
// from the bundled Node.
//
//   hang case     — fake harness prints the readiness line, listens, but never
//                   answers HTTP (a hung event loop). Expect: ready →
//                   error/unresponsive → crashed(+message, child killed), and
//                   NO self-respawn; then a manual `restart` (what the Tauri
//                   shell does) brings it back: stopped → starting → ready.
//   healthy case  — fake harness answers every probe. Expect: ready, then N
//                   intervals with NO unresponsive/crashed (the probe must not
//                   false-positive), then a clean shutdown.
//
// --self-test validates the two fake harness simulators against the bundled
// Node without the sidecar (they must print a parseable readiness line).

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  runtimeDir,
  harnessDir,
  tmpDir,
  exeSuffix,
  fail,
  ok,
  info,
} from "./lib/common.ts";
import { HANG_SCRIPT, HEALTHY_SCRIPT, isParseableReadyLine } from "./lib/heartbeat-sim.ts";

interface Event {
  type: string;
  id?: number;
  code?: string | number | null;
  message?: string;
  url?: string;
  pid?: number | null;
  state?: string;
  line?: string;
  stream?: string;
}

const sidecarPath = join(runtimeDir, `sidecar${exeSuffix}`);

// Everything this script creates, removed on ANY exit path (fail() throws
// / exits, so the catch below is the only reliable cleanup point).
const tempPaths: string[] = [];
const nodePath = process.argv.includes("--self-test")
  ? process.execPath
  : join(runtimeDir, `node${exeSuffix}`);


// ---------------------------------------------------------------------------
// Simulator sanity: the fake harness must print a parseable readiness line.
// ---------------------------------------------------------------------------
async function checkSimulator(script: string, label: string): Promise<void> {
  const child = spawn(nodePath, ["-e", script], { stdio: ["ignore", "pipe", "inherit"] });
  const firstLine = await new Promise<string>((resolve, reject) => {
    let buf = "";
    child.stdout.on("data", (c: Buffer) => {
      buf += c.toString("utf8");
      const nl = buf.indexOf("\n");
      if (nl >= 0) resolve(buf.slice(0, nl).trim());
    });
    child.on("exit", (code) => reject(new Error(`${label} exited early (${code})`)));
    setTimeout(() => reject(new Error(`${label} printed nothing in 10s`)), 10_000);
  });
  child.kill();
  if (!isParseableReadyLine(firstLine)) {
    fail(`${label} readiness line not parseable: ${firstLine}`);
  }
  ok(`self-test: ${label} prints a parseable readiness line`);
}

// ---------------------------------------------------------------------------
// Sidecar driver (minimal NDJSON plumbing, same contract as verify-runtime).
// ---------------------------------------------------------------------------
function driveSidecar(
  env: Record<string, string>,
  script: string,
  dshHome: string,
): {
  events: Event[];
  send: (line: Record<string, unknown>) => void;
  finish: () => Promise<number>;
} {
  // The sidecar contract runs `node <script> <args…>`; there is no inline
  // `-e` slot, so the fake harness is written to a .cjs file first (the repo
  // package.json is type:module — .cjs keeps require() available).
  const scriptFile = join(tmpDir, `hb-fake-${Date.now()}.cjs`);
  writeFileSync(scriptFile, script);
  tempPaths.push(scriptFile);
  const sidecar = spawn(sidecarPath, [], {
    cwd: runtimeDir,
    stdio: ["pipe", "pipe", "inherit"],
    env: { ...process.env, ...env },
  });
  const events: Event[] = [];
  let buffer = "";
  sidecar.stdout.on("data", (chunk: Buffer) => {
    buffer += chunk.toString("utf8");
    for (;;) {
      const nl = buffer.indexOf("\n");
      if (nl < 0) break;
      const line = buffer.slice(0, nl).trim();
      buffer = buffer.slice(nl + 1);
      if (!line) continue;
      try {
        events.push(JSON.parse(line) as Event);
      } catch {
        info(`non-JSON sidecar output: ${line}`);
      }
    }
  });
  const send = (obj: Record<string, unknown>) => {
    sidecar.stdin.write(JSON.stringify(obj) + "\n");
  };
  // Start the fake harness via the bundled node.
  send({
    id: 1,
    command: "start",
    node: nodePath,
    script: scriptFile,
    args: [],
    cwd: harnessDir,
    env: { DSH_HOME: dshHome },
  });
  const finish = () =>
    new Promise<number>((resolve) => {
      sidecar.stdin.end();
      sidecar.on("exit", (code) => {
        rmSync(scriptFile, { force: true });
        resolve(code ?? -1);
      });
    });
  return { events, send, finish };
}

async function waitFor(
  events: Event[],
  pred: (e: Event) => boolean,
  what: string,
  timeoutMs: number,
): Promise<Event> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const hit = events.find(pred);
    if (hit) return hit;
    if (Date.now() > deadline) {
      const tail = events.slice(-8).map((e) => e.type + (e.message ? `(${e.message})` : "")).join(", ");
      fail(`timeout waiting for ${what}; last events: ${tail}`);
    }
    await new Promise((r) => setTimeout(r, 50));
  }
}

async function hangCase(): Promise<void> {
  const dshHome = join(tmpDir, `hb-hang-${Date.now()}`);
  mkdirSync(dshHome, { recursive: true });
  tempPaths.push(dshHome);
  const { events, send, finish } = driveSidecar(
    {
      // Aggressive knobs: two 300ms probes must hang the fake child fast.
      DSH_HEARTBEAT_INTERVAL_MS: "300",
      DSH_HEARTBEAT_FAIL_LIMIT: "2",
      DSH_HEARTBEAT_READ_TIMEOUT_MS: "300",
    },
    HANG_SCRIPT,
    dshHome,
  );

  const ready = await waitFor(events, (e) => e.type === "ready", "ready", 30_000);
  ok(`hang case: ready at ${ready.url}`);

  const err = await waitFor(
    events,
    (e) => e.type === "error" && e.code === "unresponsive",
    "unresponsive error",
    30_000,
  );
  ok(`hang case: unresponsive reported — ${err.message}`);

  const crashed = await waitFor(events, (e) => e.type === "crashed", "crashed", 30_000);
  if (crashed.message !== "killed after health checks failed (unresponsive)") {
    fail(`crashed message mismatch: ${crashed.message}`);
  }
  if (typeof crashed.pid !== "number") fail(`crashed missing pid: ${JSON.stringify(crashed)}`);
  ok(`hang case: child ${crashed.pid} killed and reported with message`);

  // No self-respawn: after several intervals there must be no new ready.
  const readyCountAfter = events.filter((e) => e.type === "ready").length;
  await new Promise((r) => setTimeout(r, 1_200));
  if (events.filter((e) => e.type === "ready").length !== readyCountAfter) {
    fail("hang case: sidecar respawned on its own — restart policy must be the shell's");
  }
  ok("hang case: supervisor did not auto-respawn");

  // The shell's restart (manual command) must bring it back. Detected by
  // event COUNT, not URL inequality (the OS may legitimately reuse the port).
  const readyCountBefore = events.filter((e) => e.type === "ready").length;
  send({ id: 2, command: "restart" });
  await waitFor(
    events,
    () => events.filter((e) => e.type === "ready").length > readyCountBefore,
    "ready after restart",
    30_000,
  );
  const ready2 = events.filter((e) => e.type === "ready")[readyCountBefore];
  ok(`hang case: restart recovered — ready at ${ready2.url}`);

  // The same hang script runs again, so the watcher must re-arm for the NEW
  // generation and catch the hang a second time — proof the generation
  // tagging is wired (a stale watcher would either be ignored or mis-armed).
  await waitFor(
    events,
    () => events.filter((e) => e.type === "crashed").length >= 2,
    "second crashed (heartbeat re-armed for the new generation)",
    30_000,
  );
  const crashed2 = events.filter((e) => e.type === "crashed")[1];
  if (crashed2.message !== "killed after health checks failed (unresponsive)") {
    fail(`second crashed message mismatch: ${crashed2.message}`);
  }
  ok(`hang case: heartbeat re-armed and caught the restarted hang (${crashed2.message})`);

  const exitCode = await finish();
  if (exitCode !== 0) fail(`hang case: sidecar exited ${exitCode} after stdin EOF`);
  rmSync(dshHome, { recursive: true, force: true });
  ok("hang case: PASS");
}

async function healthyCase(): Promise<void> {
  const dshHome = join(tmpDir, `hb-healthy-${Date.now()}`);
  mkdirSync(dshHome, { recursive: true });
  tempPaths.push(dshHome);
  const { events, send, finish } = driveSidecar(
    {
      DSH_HEARTBEAT_INTERVAL_MS: "300",
      DSH_HEARTBEAT_FAIL_LIMIT: "2",
      DSH_HEARTBEAT_READ_TIMEOUT_MS: "500",
    },
    HEALTHY_SCRIPT,
    dshHome,
  );

  const ready = await waitFor(events, (e) => e.type === "ready", "ready", 30_000);
  ok(`healthy case: ready at ${ready.url}`);

  // ≥5 probe intervals of silence: any false positive must NOT appear.
  await new Promise((r) => setTimeout(r, 1_800));
  const bad = events.find(
    (e) =>
      (e.type === "error" && e.code === "unresponsive") || e.type === "crashed",
  );
  if (bad) {
    fail(`healthy case: false positive after ready — ${JSON.stringify(bad)}`);
  }
  ok("healthy case: no false positives across 6 probe intervals");

  send({ id: 2, command: "shutdown" });
  const stopped = await waitFor(events, (e) => e.type === "stopped", "stopped", 30_000);
  ok(`healthy case: clean shutdown (code ${stopped.code})`);

  const exitCode = await finish();
  if (exitCode !== 0) fail(`healthy case: sidecar exited ${exitCode} after stdin EOF`);
  rmSync(dshHome, { recursive: true, force: true });
  ok("healthy case: PASS");
}

async function main(): Promise<void> {
  if (process.argv.includes("--self-test")) {
    await checkSimulator(HANG_SCRIPT, "hang simulator");
    await checkSimulator(HEALTHY_SCRIPT, "healthy simulator");
    console.log("✓ self-test: heartbeat simulators validated");
    return;
  }
  if (!existsSync(sidecarPath)) fail("sidecar binary missing — run scripts/build-sidecar.ts first");
  await hangCase();
  await healthyCase();
  console.log("\n  PASS — liveness heartbeat smoke complete");
}

main().catch((e: Error) => {
  for (const p of tempPaths) {
    try {
      rmSync(p, { recursive: true, force: true });
    } catch {
      /* best-effort cleanup */
    }
  }
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
