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
import { join } from "node:path";
import {
  repoRoot,
  runtimeDir,
  harnessDir,
  readManifest,
  sidecarPath,
  nodePath,
  tmpDir,
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
const manifest = readManifest();

function requireFile(path: string, label: string): void {
  if (!existsSync(path)) fail(`${label} missing at ${path} — run scripts/build-sidecar.ts / download-node.ts / prepare-harness.ts first`);
}

requireFile(sidecarPath(), "sidecar binary");
requireFile(nodePath(), "bundled node");
const dshBin = join(harnessDir, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
requireFile(dshBin, "dsh entry (lib/bin.js)");

// Fresh, isolated DSH_HOME for the smoke run.
const dshHome = join(tmpDir, `smoke-dsh-home-${Date.now()}`);
mkdirSync(dshHome, { recursive: true });

const events: Event[] = [];
const log = (e: Event) => {
  events.push(e);
  if (verbose && e.type === "log") info(`[${e.stream}] ${e.line}`);
};

function waitFor(
  pred: (e: Event) => boolean,
  what: string,
  timeoutMs: number,
  minCount = 1,
): Promise<Event> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const iv = setInterval(() => {
      const hits = events.filter(pred);
      if (hits.length >= minCount) {
        clearInterval(iv);
        resolve(hits[minCount - 1]);
        return;
      }
      const fatal = events.find((e) => e.type === "error" || e.type === "crashed");
      if (fatal) {
        clearInterval(iv);
        reject(new Error(`sidecar reported ${fatal.type} while waiting for ${what}: ${JSON.stringify(fatal)}`));
        return;
      }
      if (Date.now() - start > timeoutMs) {
        clearInterval(iv);
        const tail = events.slice(-6).map((e) => `[${e.stream ?? e.type}] ${e.line ?? e.message ?? ""}`).join("\n    ");
        reject(new Error(`timeout waiting for ${what} (got ${hits.length}/${minCount});\n  last events:\n    ${tail}`));
      }
    }, 100);
  });
}

const sidecar = spawn(sidecarPath(), [], {
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
  if (res.status !== 200) fail(`GET ${url} → HTTP ${res.status}`);
  if (!/<!doctype|<html|<div/i.test(body)) fail(`GET ${url} returned a non-HTML body (${body.length} bytes)`);
  ok(`HTTP ${res.status} · ${body.length} bytes · Harness UI served`);
}

async function main(): Promise<void> {
  info(`sidecar v${manifest.sidecarVersion} · node v${manifest.nodeVersion} · @deepseek-ai/dsh ${manifest.harnessVersion}`);

  // --- boot -------------------------------------------------------------
  send({
    command: "start",
    node: nodePath(),
    script: dshBin,
    args: ["web", "--host", "127.0.0.1", "--port", "0"],
    cwd: harnessDir,
    env: { DSH_HOME: dshHome },
  });
  info("waiting for readiness line…");
  const ready = await waitFor((e) => e.type === "ready", "ready", 180_000);
  ok(`readiness captured → ${ready.url}`);
  await probe(ready.url!);

  // --- status -----------------------------------------------------------
  const statusId = send({ command: "status" });
  const status = (await waitFor((e) => e.type === "status" && e.id === statusId, "status", 5_000)) as Event;
  if (status.state !== "running" || status.url !== ready.url) {
    fail(`unexpected status reply: ${JSON.stringify(status)}`);
  }
  ok(`status → running, pid ${status.pid}`);

  // --- restart ----------------------------------------------------------
  send({ command: "restart" });
  await waitFor((e) => e.type === "stopped", "stopped (restart)", 30_000, 1);
  info("restart: tree stopped");
  // NOTE: events are cumulative — the 2nd ready is the one from the new child.
  const ready2 = await waitFor((e) => e.type === "ready", "ready (after restart)", 180_000, 2);
  ok(`restart readiness → ${ready2.url}`);
  await probe(ready2.url!);

  // --- shutdown + orphan check ------------------------------------------
  send({ command: "shutdown" });
  const stopped = (await waitFor((e) => e.type === "stopped", "stopped (shutdown)", 30_000, 2)) as Event;
  ok(`shutdown → exited code ${stopped.code}`);

  // The harness pid (from the final stopped event) must be gone now.
  let alive = true;
  try {
    process.kill(stopped.pid!, 0);
  } catch {
    alive = false;
  }
  if (alive) fail(`orphan process: harness pid ${stopped.pid} still alive after shutdown`);

  // Sidecar itself: closing stdin (parent gone) must make it exit 0.
  sidecar.stdin.end();
  const t0 = Date.now();
  while (sidecarExitCode === null && Date.now() - t0 < 30_000) {
    await new Promise((r) => setTimeout(r, 100));
  }
  if (sidecarExitCode !== 0) fail(`sidecar did not exit 0 after stdin EOF (exit ${sidecarExitCode})`);
  ok("no orphan processes; sidecar exited cleanly on parent EOF");

  rmSync(dshHome, { recursive: true, force: true });
  console.log("\n  PASS — runtime smoke complete");
}

main().catch((e: Error) => {
  console.error(`\n✗ ${e.message}`);
  try {
    sidecar.kill("SIGKILL");
  } catch {
    /* already gone */
  }
  process.exit(1);
});
