// Shared helpers for the build/verify scripts (Node >= 24 native TS).

import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";

// scripts/lib/common.ts → repo root is three levels up.
export const repoRoot = dirname(dirname(dirname(fileURLToPath(import.meta.url))));
export const runtimeDir = join(repoRoot, "src-tauri/resources/runtime");
export const harnessDir = join(runtimeDir, "harness");
export const manifestPath = join(repoRoot, "runtime", "runtime-manifest.json");
export const tmpDir = join(repoRoot, ".tmp");

export const exeSuffix = process.platform === "win32" ? ".exe" : "";

export interface RuntimeManifest {
  desktopVersion: string;
  harnessVersion: string;
  nodeVersion: string;
  sidecarVersion: string;
  nodeSha256: Record<string, string>;
}

export function readManifest(): RuntimeManifest {
  return JSON.parse(readFileSync(manifestPath, "utf8")) as RuntimeManifest;
}

export function sidecarPath(): string {
  return join(runtimeDir, `sidecar${exeSuffix}`);
}

export function nodePath(): string {
  return join(runtimeDir, `node${exeSuffix}`);
}

export function fail(message: string): never {
  console.error(`\n✗ ${message}`);
  process.exit(1);
}

export function ok(message: string): void {
  console.log(`✓ ${message}`);
}

export function info(message: string): void {
  console.log(`  ${message}`);
}

// Git for Windows may materialize a repository file with CRLF even when the
// generated source is committed with LF. Use this only for content equality
// checks; generators should continue emitting LF for a stable repository form.
export function normalizeLineEndings(text: string): string {
  return text.replace(/\r\n/g, "\n");
}

// npm version gate shared by every script that runs `npm ci` in runtime/.
// runtime/.npmrc relies on strict-allow-scripts/allow-scripts, an npm 11
// feature: OLDER npm silently ignores unknown config keys (fail open — every
// install script runs unreviewed), and a FUTURE major might rename or drop
// the keys (also fail open). Both bounds are therefore deny-level.
//
// DSH_ALLOW_NPM_MAJOR=<n> is the reviewed override channel: after a human
// verifies that npm <n> still gates install scripts via those keys, a
// release can proceed by setting it explicitly. Everything else fails.
export function assertNpmInAuditedRange(): string {
  const res = spawnSync("npm", ["--version"], {
    encoding: "utf8",
    shell: process.platform === "win32",
  });
  const version = (res.stdout ?? "").trim();
  const [major = 0, minor = 0] = version
    .split(".")
    .map((p) => Number.parseInt(p, 10) || 0);
  if (res.status !== 0 || major < 11 || (major === 11 && minor < 17)) {
    fail(`npm ${version || "not found"} too old: strict-allow-scripts requires >= 11.17`);
  }
  if (major > 11 && process.env.DSH_ALLOW_NPM_MAJOR !== String(major)) {
    fail(
      `npm ${version} is newer than the audited major (11): verify that ` +
        `strict-allow-scripts/allow-scripts still gates install scripts in runtime/.npmrc, ` +
        `then set DSH_ALLOW_NPM_MAJOR=${major} for this release (or widen the check).`,
    );
  }
  return version;
}
