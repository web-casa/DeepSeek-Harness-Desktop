// Download the pinned Node.js runtime binary into src-tauri/resources/runtime/.
//
// The version comes from runtime/runtime-manifest.json (single source of
// truth). Extraction reuses the system `tar` (bsdtar on Windows/macOS handles
// .zip/.tar.gz, GNU tar on Linux handles .tar.xz), so the script has zero
// npm dependencies.

import { createWriteStream, existsSync, mkdirSync, rmSync, chmodSync, copyFileSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { readManifest, runtimeDir, nodePath, tmpDir, fail, ok, info } from "./lib/common.ts";

interface Dist {
  file: string;
  bin: string; // path of the binary inside the archive
}

function distFor(): Dist {
  const v = readManifest().nodeVersion;
  const base = `node-v${v}`;
  const map: Record<string, Dist> = {
    "win32-x64": { file: `${base}-win-x64.zip`, bin: `${base}-win-x64/node.exe` },
    "darwin-arm64": { file: `${base}-darwin-arm64.tar.gz`, bin: `${base}-darwin-arm64/bin/node` },
    "darwin-x64": { file: `${base}-darwin-x64.tar.gz`, bin: `${base}-darwin-x64/bin/node` },
    "linux-x64": { file: `${base}-linux-x64.tar.xz`, bin: `${base}-linux-x64/bin/node` },
    "linux-arm64": { file: `${base}-linux-arm64.tar.xz`, bin: `${base}-linux-arm64/bin/node` },
  };
  const key = `${process.platform}-${process.arch}`;
  const dist = map[key];
  if (!dist) fail(`unsupported platform/arch: ${key} (targets: win32-x64, darwin-arm64, darwin-x64, linux-x64, linux-arm64)`);
  return dist;
}

async function download(url: string, dest: string): Promise<void> {
  info(`downloading ${url}`);
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok || !res.body) fail(`download failed: HTTP ${res.status}`);
  const total = Number(res.headers.get("content-length") ?? 0);
  const reader = res.body.getReader();
  const out = createWriteStream(dest);
  let received = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    received += value.byteLength;
    out.write(Buffer.from(value));
    if (total > 0 && received % (16 * 1024 * 1024) < 64 * 1024) {
      process.stdout.write(`\r  ${((received / total) * 100).toFixed(0)}% (${(received / 1048576).toFixed(0)} MB)`);
    }
  }
  out.end();
  await new Promise<void>((resolve, reject) => {
    out.on("finish", resolve);
    out.on("error", reject);
  });
  process.stdout.write("\r");
}

function run(cmd: string, args: string[]): void {
  const res = spawnSync(cmd, args, { stdio: "inherit" });
  if (res.status !== 0) fail(`${cmd} ${args.join(" ")} exited with ${res.status}`);
}

const manifest = readManifest();
const platformKey = `${process.platform}-${process.arch}`;
const dist = distFor();
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

if (!existsSync(archive)) {
  await download(`https://nodejs.org/dist/v${v}/${dist.file}`, archive);
} else {
  info(`reusing cached archive ${dist.file}`);
}

const expectedSha256 = manifest.nodeSha256[platformKey];
if (!expectedSha256) fail(`runtime-manifest.json is missing nodeSha256 for ${platformKey}`);
const actualSha256 = createHash("sha256").update(readFileSync(archive)).digest("hex");
if (actualSha256.toLowerCase() !== expectedSha256.toLowerCase()) {
  fail(`SHA-256 mismatch for ${dist.file}: expected ${expectedSha256}, got ${actualSha256}`);
}

info(`extracting ${dist.file}`);
run("tar", ["-xf", archive, "-C", extractDir]);

const extracted = join(extractDir, dist.bin);
if (!existsSync(extracted)) fail(`binary not found in archive at ${dist.bin}`);
copyFileSync(extracted, nodePath());
if (process.platform !== "win32") chmodSync(nodePath(), 0o755);

// Verify the binary actually runs.
const probe = spawnSync(nodePath(), ["--version"], { encoding: "utf8" });
if (probe.status !== 0 || !probe.stdout.includes(`v${v}`)) {
  fail(`bundled node failed verification: ${probe.stderr ?? probe.stdout}`);
}

rmSync(extractDir, { recursive: true, force: true });
ok(`node v${v} ready at ${nodePath()}`);
