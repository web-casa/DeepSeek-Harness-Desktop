// Shared helpers for the build/verify scripts (Node >= 24 native TS).

import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync } from "node:fs";

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
