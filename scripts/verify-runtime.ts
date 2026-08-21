// End-to-end runtime smoke test: the exact P0 acceptance chain.
//
//   dsh-sidecar → bundled node → dsh web --port 0
//   → readiness line → HTTP 200 → shutdown → no orphan process
//
// Also exercises `restart`. Runs against src-tauri/resources/runtime/ — the
// same files that end up inside the app bundle. CI runs this on all three
// platforms; `cargo test` covers the pure logic.

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import {
  repoRoot,
  readManifest,
  tmpDir,
  exeSuffix,
  fail,
  ok,
  info,
} from "./lib/common.ts";

interface Event {
  type: string;
  id?: number;
  state?: string;
  url?: string;
  pid?: number;
  code?: number | null;
  line?: string;
  stream?: string;
  message?: string;
  ok?: boolean;
}

const verbose = process.argv.includes("--verbose");
const runtimeDirArg = process.argv.indexOf("--runtime-dir");
if (runtimeDirArg >= 0 && !process.argv[runtimeDirArg + 1]) {
  fail("--runtime-dir requires a path");
}
const runtimeDir = runtimeDirArg >= 0
  ? resolve(process.argv[runtimeDirArg + 1])
  : join(repoRoot, "src-tauri/resources/runtime");
const harnessDir = join(runtimeDir, "harness");
const sidecarPath = join(runtimeDir, `sidecar${exeSuffix}`);
const nodePath = join(runtimeDir, `node${exeSuffix}`);
const manifest = readManifest();

function requireFile(path: string, label: string): void {
  if (!existsSync(path)) fail(`${label} missing at ${path} — run scripts/build-sidecar.ts / download-node.ts / prepare-harness.ts first`);
}

function runtimeFail(message: string): never {
  throw new Error(message);
}

requireFile(sidecarPath, "sidecar binary");
requireFile(nodePath, "bundled node");
const dshBin = join(harnessDir, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
requireFile(dshBin, "dsh entry (lib/bin.js)");

// Tauri can return resource paths in Win32's `\\?\` (verbatim) form. Node
// currently cannot resolve a main entrypoint in that form: it reduces it to
// the bare drive (`C:`) and exits with EISDIR. Deliberately pass this form to
// the real sidecar on Windows so the smoke test proves its launch boundary
// converts only the Node-facing paths back to ordinary DOS/UNC notation.
function nodeLaunchPath(path: string): string {
  if (process.platform !== "win32") return path;
  if (path.startsWith("\\\\?\\")) return path;
  if (path.startsWith("\\\\")) return `\\\\?\\UNC\\${path.slice(2)}`;
  if (/^[A-Za-z]:[\\/]/.test(path)) return `\\\\?\\${path}`;
  runtimeFail(`Windows runtime path is not absolute: ${path}`);
}

// Fresh, isolated DSH_HOME for the smoke run.
const dshHome = join(tmpDir, `smoke-dsh-home-${Date.now()}`);
mkdirSync(dshHome, { recursive: true });

const events: Event[] = [];
const log = (e: Event) => {
  events.push(e);
  if (verbose && e.type === "log") info(`[${e.stream}] ${e.line}`);
};

function lastLogs(count: number): string {
  const logEvents = events.filter((e) => e.type === "log").slice(-count);
  if (logEvents.length === 0) return "（无日志）";
  return logEvents.map((e) => `[${e.stream}] ${e.line}`).join("\n    ");
}

// Waits for predicate matches BEYOND a baseline: baseline = the number of
// matches already accumulated before the command that should produce the new
// event. Counting globally (minCount=N) silently breaks — and can assert the
// WRONG pid in the orphan check — if the sidecar ever emits extra events of
// the same type (e.g. a multi-stage stop). Baseline makes the assertion
// "a NEW stopped/ready appeared after MY command", independent of history.
function waitFor(
  pred: (e: Event) => boolean,
  what: string,
  timeoutMs: number,
  minCount = 1,
  baseline = 0,
): Promise<Event> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const iv = setInterval(() => {
      const hits = events.filter(pred);
      if (hits.length >= baseline + minCount) {
        clearInterval(iv);
        resolve(hits[baseline + minCount - 1]);
        return;
      }
      const fatal = events.find((e) => e.type === "error" || e.type === "crashed");
      if (fatal) {
        clearInterval(iv);
        reject(
          new Error(
            `sidecar reported ${fatal.type} while waiting for ${what}: ${JSON.stringify(fatal)}\n  recent child logs:\n    ${lastLogs(25)}`,
          ),
        );
        return;
      }
      if (Date.now() - start > timeoutMs) {
        clearInterval(iv);
        const tail = events.slice(-6).map((e) => `[${e.stream ?? e.type}] ${e.line ?? e.message ?? ""}`).join("\n    ");
        reject(
          new Error(
            `timeout waiting for ${what} (got ${hits.length - baseline}/${minCount} after baseline ${baseline});\n  last events:\n    ${tail}\n  recent child logs:\n    ${lastLogs(25)}`,
          ),
        );
      }
    }, 100);
  });
}

const sidecar = spawn(sidecarPath, [], {
  cwd: runtimeDir,
  stdio: ["pipe", "pipe", "inherit"],
});

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
      log(JSON.parse(line) as Event);
    } catch {
      info(`non-JSON sidecar output: ${line}`);
    }
  }
});

let nextId = 1;
function send(obj: Record<string, unknown>): number {
  const id = nextId++;
  sidecar.stdin.write(JSON.stringify({ id, ...obj }) + "\n");
  return id;
}

let sidecarExitCode: number | null = null;
sidecar.on("exit", (code) => {
  sidecarExitCode = code;
});

async function probe(url: string): Promise<void> {
  const res = await fetch(url, { redirect: "follow" });
  const body = await res.text();
  if (res.status !== 200) runtimeFail(`GET ${url} → HTTP ${res.status}`);
  if (!/<!doctype|<html|<div/i.test(body)) runtimeFail(`GET ${url} returned a non-HTML body (${body.length} bytes)`);
  ok(`HTTP ${res.status} · ${body.length} bytes · Harness UI served`);
}

async function main(): Promise<void> {
  info(`sidecar v${manifest.sidecarVersion} · node v${manifest.nodeVersion} · @deepseek-ai/dsh ${manifest.harnessVersion}`);

  // --- boot -------------------------------------------------------------
  send({
    command: "start",
    node: nodeLaunchPath(nodePath),
    script: nodeLaunchPath(dshBin),
    args: ["web", "--no-open", "--host", "127.0.0.1", "--port", "0"],
    cwd: nodeLaunchPath(harnessDir),
    // Keep this env in lockstep with src-tauri/src/harness/mod.rs
    // (start_harness): the smoke must exercise the production contract.
    env: { DSH_HOME: nodeLaunchPath(dshHome), DSH_TELEMETRY_DISABLED: "1" },
  });
  info("waiting for readiness line…");
  const ready = await waitFor((e) => e.type === "ready", "ready", 180_000);
  ok(`readiness captured → ${ready.url}`);
  await probe(ready.url!);

  // --- status -----------------------------------------------------------
  const statusId = send({ command: "status" });
  const status = (await waitFor((e) => e.type === "status" && e.id === statusId, "status", 5_000)) as Event;
  if (typeof status.pid !== "number") runtimeFail(`status reply missing numeric pid: ${JSON.stringify(status)}`);
  if (status.state !== "running" || status.url !== ready.url) {
    runtimeFail(`unexpected status reply: ${JSON.stringify(status)}`);
  }
  ok(`status → running, pid ${status.pid}`);

  // --- restart ----------------------------------------------------------
  // Baselines are snapshots BEFORE the command: only events that appear
  // AFTER the restart/shutdown request count, so extra stopped/ready events
  // emitted by a future sidecar cannot shift the assertion onto the wrong
  // pid (and produce a false-green orphan check).
  const stoppedBeforeRestart = events.filter((e) => e.type === "stopped").length;
  const readyBeforeRestart = events.filter((e) => e.type === "ready").length;
  send({ command: "restart" });
  await waitFor((e) => e.type === "stopped", "stopped (restart)", 30_000, 1, stoppedBeforeRestart);
  info("restart: tree stopped");
  const ready2 = await waitFor(
    (e) => e.type === "ready",
    "ready (after restart)",
    180_000,
    1,
    readyBeforeRestart,
  );
  ok(`restart readiness → ${ready2.url}`);
  await probe(ready2.url!);

  // --- shutdown + orphan check ------------------------------------------
  const stoppedBeforeShutdown = events.filter((e) => e.type === "stopped").length;
  send({ command: "shutdown" });
  const stopped = (await waitFor(
    (e) => e.type === "stopped",
    "stopped (shutdown)",
    30_000,
    1,
    stoppedBeforeShutdown,
  )) as Event;
  if (typeof stopped.pid !== "number") runtimeFail(`stopped reply missing numeric pid: ${JSON.stringify(stopped)}`);
  ok(`shutdown → exited code ${stopped.code}`);

  // The harness pid (from the final stopped event) must be gone now.
  // signal 0 probes existence without delivering anything; on Windows the
  // signal argument is ignored by the OS but the ESRCH existence check is
  // identical (process.kill throws when the pid no longer exists).
  let alive = true;
  try {
    process.kill(stopped.pid, 0);
  } catch {
    alive = false;
  }
  if (alive) runtimeFail(`orphan process: harness pid ${stopped.pid} still alive after shutdown`);

  if (process.platform !== "win32") {
    let groupError: NodeJS.ErrnoException | undefined;
    try {
      process.kill(-stopped.pid, 0);
    } catch (e) {
      groupError = e as NodeJS.ErrnoException;
    }
    if (!groupError) {
      runtimeFail(`orphan process group: harness process group ${stopped.pid} still alive after shutdown`);
    }
    if (groupError.code !== "ESRCH") {
      runtimeFail(`could not verify harness process group ${stopped.pid}: ${groupError.message}`);
    }
  }

  // Sidecar itself: closing stdin (parent gone) must make it exit 0.
  sidecar.stdin.end();
  const t0 = Date.now();
  while (sidecarExitCode === null && Date.now() - t0 < 30_000) {
    await new Promise((r) => setTimeout(r, 100));
  }
  if (sidecarExitCode !== 0) runtimeFail(`sidecar did not exit 0 after stdin EOF (exit ${sidecarExitCode})`);
  ok("no orphan processes; sidecar exited cleanly on parent EOF");

  rmSync(dshHome, { recursive: true, force: true });
  console.log("\n  PASS — runtime smoke complete");
}

async function waitForSidecarExit(timeoutMs: number): Promise<void> {
  const t0 = Date.now();
  while (sidecarExitCode === null && Date.now() - t0 < timeoutMs) {
    await new Promise((r) => setTimeout(r, 100));
  }
}

main().catch(async (e: Error) => {
  try {
    try {
      sidecar.stdin.end();
    } catch {
      /* sidecar stdin already closed */
    }
    await waitForSidecarExit(10_000);
    if (sidecarExitCode === null) {
      try {
        sidecar.kill("SIGKILL");
      } catch {
        /* sidecar already exited */
      }
    }
  } finally {
    rmSync(dshHome, { recursive: true, force: true });
  }
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
