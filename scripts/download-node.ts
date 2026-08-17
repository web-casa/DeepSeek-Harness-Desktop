// Download the pinned Node.js runtime binary into src-tauri/resources/runtime/.
//
// The version comes from runtime/runtime-manifest.json (single source of
// truth). Extraction reuses the system `tar` (bsdtar on Windows/macOS handles
// .zip/.tar.gz, GNU tar on Linux handles .tar.xz), so the script has zero
// npm dependencies.

import { createWriteStream, existsSync, mkdirSync, rmSync, renameSync, chmodSync, copyFileSync, createReadStream } from "node:fs";
import { once } from "node:events";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { readManifest, runtimeDir, nodePath, tmpDir, fail, ok, info } from "./lib/common.ts";

interface Dist {
  file: string;
  bin: string; // path of the binary inside the archive
}

function archArg(): string | undefined {
  const i = process.argv.indexOf("--arch");
  return i >= 0 ? process.argv[i + 1] : undefined;
}

function skipProbeArg(): boolean {
  return process.argv.includes("--skip-probe");
}

function distFor(): Dist {
  const v = readManifest().nodeVersion;
  const base = `node-v${v}`;
  const map: Record<string, Dist> = {
    "win32-x64": { file: `${base}-win-x64.zip`, bin: `${base}-win-x64/node.exe` },
    "win32-arm64": { file: `${base}-win-arm64.zip`, bin: `${base}-win-arm64/node.exe` },
    "darwin-arm64": { file: `${base}-darwin-arm64.tar.gz`, bin: `${base}-darwin-arm64/bin/node` },
    "darwin-x64": { file: `${base}-darwin-x64.tar.gz`, bin: `${base}-darwin-x64/bin/node` },
    "linux-x64": { file: `${base}-linux-x64.tar.xz`, bin: `${base}-linux-x64/bin/node` },
    "linux-arm64": { file: `${base}-linux-arm64.tar.xz`, bin: `${base}-linux-arm64/bin/node` },
  };
  const arch = archArg() ?? process.arch;
  const key = `${process.platform}-${arch}`;
  const dist = map[key];
  if (!dist) fail(`unsupported platform/arch: ${key} (targets: win32-x64, win32-arm64, darwin-arm64, darwin-x64, linux-x64, linux-arm64)`);
  return dist;
}

const DOWNLOAD_TIMEOUT_MS = 5 * 60_000;
const DOWNLOAD_RETRIES = 3;

async function download(url: string, dest: string): Promise<void> {
  info(`downloading ${url}`);
  let lastError: unknown;
  for (let attempt = 1; attempt <= DOWNLOAD_RETRIES; attempt++) {
    try {
      await downloadOnce(url, dest);
      return;
    } catch (e) {
      lastError = e;
      if (attempt < DOWNLOAD_RETRIES) {
        info(`download attempt ${attempt} failed (${(e as Error).message}); retrying…`);
        await new Promise((r) => setTimeout(r, 2000 * attempt));
      }
    }
  }
  fail(`download failed after ${DOWNLOAD_RETRIES} attempts: ${(lastError as Error).message}`);
}

async function downloadOnce(url: string, dest: string): Promise<void> {
  const res = await fetch(url, {
    redirect: "follow",
    signal: AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS),
  });
  if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
  const total = Number(res.headers.get("content-length") ?? 0);
  const reader = res.body.getReader();
  const out = createWriteStream(dest);
  // ONE persistent error listener (per-wait `once("error")` calls would stack
  // listeners and trip the MaxListeners warning over a long download).
  const streamError = new Promise<never>((_, reject) => {
    out.on("error", reject);
  });
  // Mark the rejection as observed: an early stream error (e.g. open failure
  // before the first write, arriving while `reader.read()` is pending) must
  // not crash the process as an unhandled rejection — it has to reach the
  // retry loop through the races below instead.
  streamError.catch(() => {});
  let received = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      received += value.byteLength;
      const buf = Buffer.from(value);
      // Respect backpressure: a slow disk must not buffer the whole archive.
      if (!out.write(buf)) {
        await Promise.race([once(out, "drain"), streamError]);
      }
      if (total > 0 && received % (16 * 1024 * 1024) < 64 * 1024) {
        process.stdout.write(`\r  ${((received / total) * 100).toFixed(0)}% (${(received / 1048576).toFixed(0)} MB)`);
      }
    }
    out.end();
    await Promise.race([once(out, "finish"), streamError]);
  } catch (e) {
    out.destroy();
    throw e;
  }
  process.stdout.write("\r");
}

function run(cmd: string, args: string[]): void {
  const res = spawnSync(cmd, args, { stdio: "inherit" });
  if (res.status !== 0) fail(`${cmd} ${args.join(" ")} exited with ${res.status}`);
}

const manifest = readManifest();
const platformKey = `${process.platform}-${archArg() ?? process.arch}`;
const dist = distFor();
const skipProbe = skipProbeArg() || archArg() !== undefined && archArg() !== process.arch;
const v = manifest.nodeVersion;
// Scratch stays OUTSIDE src-tauri/resources/runtime — everything in there
// gets bundled into the app. Only the final binary is copied in.
const scratch = join(tmpDir, "node-dist");
const archive = join(scratch, dist.file);
const extractDir = join(scratch, "extract");

mkdirSync(runtimeDir, { recursive: true });
mkdirSync(scratch, { recursive: true });
rmSync(extractDir, { recursive: true, force: true });
mkdirSync(extractDir, { recursive: true });

// Download to a .part file and rename on success: an interrupted download
// must never poison the cache (a partial archive would fail SHA-256 forever).
const partFile = `${archive}.part`;
rmSync(partFile, { force: true });
if (!existsSync(archive)) {
  await download(`https://nodejs.org/dist/v${v}/${dist.file}`, partFile);
  renameSync(partFile, archive);
} else {
  info(`reusing cached archive ${dist.file}`);
}

const expectedSha256 = manifest.nodeSha256[platformKey];
if (!expectedSha256) fail(`runtime-manifest.json is missing nodeSha256 for ${platformKey}`);

// Stream the archive through the hash instead of loading it into memory.
async function sha256File(path: string): Promise<string> {
  const hash = createHash("sha256");
  const stream = createReadStream(path);
  for await (const chunk of stream) {
    hash.update(chunk);
  }
  return hash.digest("hex");
}
const actualSha256 = await sha256File(archive);
if (actualSha256.toLowerCase() !== expectedSha256.toLowerCase()) {
  // Drop the bad archive so the next run re-downloads instead of failing
  // on the poisoned cache again.
  rmSync(archive, { force: true });
  fail(`SHA-256 mismatch for ${dist.file}: expected ${expectedSha256}, got ${actualSha256} (cached archive removed; rerun to re-download)`);
}

info(`extracting ${dist.file}`);
run("tar", ["-xf", archive, "-C", extractDir]);

const extracted = join(extractDir, dist.bin);
if (!existsSync(extracted)) fail(`binary not found in archive at ${dist.bin}`);
copyFileSync(extracted, nodePath());
if (process.platform !== "win32") chmodSync(nodePath(), 0o755);

// Verify the binary actually runs when it targets the current host.
if (!skipProbe) {
  const probe = spawnSync(nodePath(), ["--version"], { encoding: "utf8" });
  if (probe.status !== 0 || !probe.stdout.includes(`v${v}`)) {
    fail(`bundled node failed verification: ${probe.stderr ?? probe.stdout}`);
  }
} else {
  info(`skipping node probe for foreign arch ${archArg() ?? process.arch}`);
}

rmSync(extractDir, { recursive: true, force: true });
ok(`node v${v} ready at ${nodePath()}`);
