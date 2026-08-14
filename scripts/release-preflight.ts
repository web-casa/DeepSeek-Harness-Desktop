// Release preflight: run before tagging (or as the first CI build step).
//
// Deny-level checks: version alignment across the four version files, the
// sidecar/harness pins, the node checksum table (exact 5-platform set, 64-hex
// each), .nvmrc consistency, npm >= 11.17 (script allowlist precondition).
// Tag binding: --expect-tag vX.Y.Z requires ref_name == "v${desktopVersion}".
// Warn-level (skipped in CI): dirty worktree, wrong branch, existing tag.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { repoRoot, readManifest, fail, ok, info } from "./lib/common.ts";

const PLATFORM_KEYS = ["win32-x64", "darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64"];
const SHA256_RE = /^[0-9a-f]{64}$/;

function readJson(path: string): Record<string, unknown> {
  return JSON.parse(readFileSync(join(repoRoot, path), "utf8")) as Record<string, unknown>;
}

function cargoVersion(path: string): string {
  const text = readFileSync(join(repoRoot, path), "utf8");
  const m = /^version = "([^"]+)"/m.exec(text);
  if (!m) fail(`could not parse version from ${path}`);
  return m[1];
}

const manifest = readManifest();

// --- deny: version alignment ---------------------------------------------
const desktopVersion = manifest.desktopVersion;
const rootPkg = readJson("package.json") as { version?: string };
const tauriConf = readJson("src-tauri/tauri.conf.json") as { version?: string };
const tauriCargo = cargoVersion("src-tauri/Cargo.toml");
const sidecarCargo = cargoVersion("crates/dsh-sidecar/Cargo.toml");
const runtimePkg = readJson("runtime/package.json") as {
  dependencies?: Record<string, string>;
};

const versionFiles: [string, string | undefined][] = [
  ["runtime/runtime-manifest.json", desktopVersion],
  ["package.json", rootPkg.version],
  ["src-tauri/Cargo.toml", tauriCargo],
  ["src-tauri/tauri.conf.json", tauriConf.version],
];
for (const [file, version] of versionFiles) {
  if (version !== desktopVersion) {
    fail(`version drift: ${file} says ${version}, manifest says ${desktopVersion}`);
  }
}
ok(`desktop version aligned: ${desktopVersion} (manifest/package.json/Cargo.toml/tauri.conf.json)`);

if (sidecarCargo !== manifest.sidecarVersion) {
  fail(`sidecar version drift: Cargo.toml ${sidecarCargo} != manifest ${manifest.sidecarVersion}`);
}
ok(`sidecar version aligned: ${sidecarCargo}`);

const harnessPin = runtimePkg.dependencies?.["@deepseek-ai/dsh"];
if (harnessPin !== manifest.harnessVersion) {
  fail(`harness pin drift: runtime/package.json pins ${harnessPin}, manifest says ${manifest.harnessVersion}`);
}
ok(`harness pin aligned: ${harnessPin}`);

// --- deny: node checksums + .nvmrc ---------------------------------------
const nvmrc = readFileSync(join(repoRoot, ".nvmrc"), "utf8").trim();
if (nvmrc !== manifest.nodeVersion) {
  fail(`.nvmrc says ${nvmrc} but manifest says ${manifest.nodeVersion}`);
}
ok(`.nvmrc aligned: ${nvmrc}`);

const checksums = manifest.nodeSha256;
for (const key of PLATFORM_KEYS) {
  if (typeof checksums[key] !== "string" || !SHA256_RE.test(checksums[key])) {
    fail(`nodeSha256["${key}"] missing or not a 64-hex checksum`);
  }
}
const extraKeys = Object.keys(checksums).filter((k) => !PLATFORM_KEYS.includes(k));
if (extraKeys.length > 0) {
  fail(`nodeSha256 has unexpected keys: ${extraKeys.join(", ")}`);
}
ok(`node checksum table covers exactly ${PLATFORM_KEYS.length} platforms (64-hex each)`);

// --- deny: npm version (allowlist precondition) ---------------------------
const npmRes = spawnSync("npm", ["--version"], { encoding: "utf8", shell: process.platform === "win32" });
const npmVersion = (npmRes.stdout ?? "").trim();
const [vMajor = 0, vMinor = 0] = npmVersion.split(".").map((p) => Number.parseInt(p, 10) || 0);
if (npmRes.status !== 0 || vMajor < 11 || (vMajor === 11 && vMinor < 17)) {
  fail(`npm ${npmVersion || "not found"} too old: strict-allow-scripts requires >= 11.17`);
}
ok(`npm ${npmVersion} supports strict-allow-scripts`);

// --- deny: tag binding ------------------------------------------------------
const tagFlag = process.argv.indexOf("--expect-tag");
if (tagFlag >= 0) {
  const expected = process.argv[tagFlag + 1];
  if (!expected) fail("--expect-tag requires a value");
  if (expected !== `v${desktopVersion}`) {
    fail(`tag ${expected} does not match desktop version v${desktopVersion}; refusing to publish`);
  }
  ok(`tag bound to version: ${expected}`);
}

// --- warn: git state (skipped in CI) ---------------------------------------
if (!process.env.CI) {
  const git = (args: string[]) =>
    spawnSync("git", args, { encoding: "utf8", cwd: repoRoot });
  const status = git(["status", "--porcelain"]).stdout ?? "";
  if (status.trim() !== "") {
    info(`⚠ worktree is not clean:\n${status}`);
  } else {
    ok("worktree clean");
  }
  const branch = (git(["branch", "--show-current"]).stdout ?? "").trim();
  if (branch !== "main" && branch !== "master") {
    info(`⚠ current branch is ${branch || "(detached)"}, not main`);
  } else {
    ok(`on branch ${branch}`);
  }
  const tags = (git(["tag", "--list", `v${desktopVersion}`]).stdout ?? "").trim();
  if (tags !== "") {
    info(`⚠ tag v${desktopVersion} already exists locally`);
  } else {
    ok("target tag does not exist yet");
  }
}

console.log(`\n✓ preflight passed — desktop ${desktopVersion} · harness ${manifest.harnessVersion} · node ${manifest.nodeVersion} · sidecar ${manifest.sidecarVersion}`);
