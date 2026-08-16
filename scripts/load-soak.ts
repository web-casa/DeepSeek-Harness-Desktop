// Load soak for the liveness heartbeat: run the REAL harness under CPU
// contention and assert the production-default heartbeat never false-kills.
//
// Honest scope (see SECURITY.md): CPU contention probes the "probe timeout
// under system load" path; a blocked DSH event loop (the other hang shape) is
// covered by verify-heartbeat.ts's hang case. Both are required, neither
// substitutes for the other.
//
//   node scripts/load-soak.ts --duration-min 30 --cpu-burn 4   # release gate
//   node scripts/load-soak.ts --self-test                      # 20s machinery check
//
// Zero npm dependencies; uses the bundled Node for both the harness and the
// CPU burners (cross-platform: no shell `yes` dependency).

import { spawn } from "node:child_process";
import http from "node:http";
import { mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { runtimeDir, harnessDir, tmpDir, exeSuffix, fail, ok, info } from "./lib/common.ts";

interface Event {
  type: string;
  code?: string | number | null;
  message?: string;
  url?: string;
  pid?: number | null;
  line?: string;
  stream?: string;
}

const sidecarPath = join(runtimeDir, `sidecar${exeSuffix}`);
const nodePath = join(runtimeDir, `node${exeSuffix}`);

function argNum(name: string, fallback: number): number {
  const i = process.argv.indexOf(name);
  const v = i >= 0 ? Number(process.argv[i + 1]) : NaN;
  return Number.isFinite(v) && v > 0 ? v : fallback;
}

async function main(): Promise<void> {
  if (process.argv.includes("--self-test")) {
    const pass = await soak(0.35, 1);
    if (!pass) fail("self-test: load-soak machinery failed");
    ok("self-test: load-soak machinery PASS");
    return;
  }
  const minutes = argNum("--duration-min", 30);
  const burners = Math.trunc(argNum("--cpu-burn", 4));
  const pass = await soak(minutes, burners);
  if (!pass) fail("load soak failed — see logs above");
  console.log("\n  PASS — load soak complete");
}

async function soak(minutes: number, burners: number): Promise<boolean> {
  const durationMs = minutes * 60_000;
  const dshHome = join(tmpDir, `soak-home-${Date.now()}`);
  mkdirSync(dshHome, { recursive: true });

  const sidecar = spawn(sidecarPath, [], {
    cwd: runtimeDir,
    stdio: ["pipe", "pipe", "inherit"],
    // PRODUCTION-default heartbeat knobs on purpose — no overrides.
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
        /* non-JSON sidecar output */
      }
    }
  });

  // CPU burners: the bundled node busy-loops (no shell dependency).
  const burnerProcs: ReturnType<typeof spawn>[] = [];
  for (let i = 0; i < burners; i++) {
    burnerProcs.push(spawn(nodePath, ["-e", "for(;;){}"], { stdio: "ignore" }));
  }

  let readyUrl: string | null = null;
  let lastProbeMs: number | null = null;
  const t0 = Date.now();

  // The verdict resolves EXACTLY once — from the soak tick or from an early
  // sidecar exit (failure). main() awaits it before printing PASS, so a
  // premature "PASS" can never be printed while the soak is still running.
  let resolveVerdict: ((pass: boolean) => void) | null = null;
  const verdict = new Promise<boolean>((resolve) => {
    resolveVerdict = resolve;
  });
  let finished = false;
  const finish = (pass: boolean) => {
    if (finished) return;
    finished = true;
    clearInterval(timer);
    for (const p of burnerProcs) p.kill();
    try {
      sidecar.stdin.write(JSON.stringify({ id: 2, command: "shutdown" }) + "\n");
    } catch {
      /* sidecar already gone */
    }
    setTimeout(() => {
      try {
        sidecar.stdin.end();
      } catch {
        /* ignore */
      }
      rmSync(dshHome, { recursive: true, force: true });
      resolveVerdict?.(pass);
    }, 15_000);
  };

  const probe = () =>
    new Promise<number>((resolve) => {
      if (!readyUrl) {
        resolve(-1);
        return;
      }
      const start = Date.now();
      const req = http.get(readyUrl, (res) => {
        res.resume();
        resolve(Date.now() - start);
      });
      req.on("error", () => resolve(-1));
      req.setTimeout(3000, () => {
        req.destroy();
        resolve(-1);
      });
    });

  const timer = setInterval(async () => {
    const mins = ((Date.now() - t0) / 60_000).toFixed(1);
    const latency = await probe();
    lastProbeMs = latency;
    const ready = events.filter((e) => e.type === "ready").length;
    const bad = events.filter(
      (e) =>
        (e.type === "error" && e.code === "unresponsive") || e.type === "crashed",
    ).length;
    info(`[soak ${mins}m] ready=${ready} bad=${bad} probe=${latency}ms`);
    if (Date.now() - t0 >= durationMs) {
      finish(bad === 0);
    }
  }, 10_000);

  sidecar.on("exit", () => {
    // Exited before the verdict: the soak failed (finish()'s shutdown path
    // is the only legitimate way out).
    finish(false);
  });

  sidecar.stdin.write(
    JSON.stringify({
      id: 1,
      command: "start",
      node: nodePath,
      script: join(harnessDir, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js"),
      args: ["web", "--host", "127.0.0.1", "--port", "0"],
      cwd: harnessDir,
      env: { DSH_HOME: dshHome, DSH_TELEMETRY_DISABLED: "1" },
    }) + "\n",
  );

  // Wait for readiness before starting the clock.
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    const ready = events.find((e) => e.type === "ready");
    if (ready?.url) {
      readyUrl = ready.url;
      ok(`ready at ${readyUrl} — soak clock started (${minutes}min, ${burners} burners)`);
      const pass = await verdict;
      if (!pass) fail(`soak FAILED: heartbeat fired under load (last probe ${lastProbeMs}ms)`);
      ok(`soak finished: ${minutes}min, ${burners} burners, no false kill`);
      return pass;
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  finish(false);
  await verdict;
  fail("harness did not become ready within 120s");
}

main().catch((e: Error) => {
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
