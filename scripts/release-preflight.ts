// Release preflight: run before tagging (or as the first CI build step).
//
// Deny-level checks: version alignment across the version files (including the
// runtime lockfile's resolved dsh version), the sidecar/harness pins, the node
// checksum table (exact 6-platform set, 64-hex each), .nvmrc consistency,
// npm 11.17..11.x (script allowlist precondition, both bounds).
// Tag binding: --expect-tag vX.Y.Z requires ref_name == "v${desktopVersion}".
// Warn-level (skipped in CI): dirty worktree, wrong branch, existing tag.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import {
  repoRoot,
  readManifest,
  fail,
  ok,
  info,
  assertNpmInAuditedRange,
} from "./lib/common.ts";

const PLATFORM_KEYS = ["win32-x64", "win32-arm64", "darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64"];
// Case-insensitive: the checksums we publish are lowercase, but a manual edit
// must not slip through just because it used uppercase hex.
const SHA256_RE = /^[0-9a-fA-F]{64}$/;

function readJson(path: string): Record<string, unknown> {
  return JSON.parse(readFileSync(join(repoRoot, path), "utf8")) as Record<string, unknown>;
}

function cargoVersion(path: string): string {
  const text = readFileSync(join(repoRoot, path), "utf8");
  const m = /^version = "([^"]+)"/m.exec(text);
  if (!m) fail(`could not parse version from ${path}`);
  return m[1];
}

/// Parse the version pin of a `dep = { path = "...", version = "..." }`
/// dependency declaration (used for the workspace dsh-sidecar pin).
function cargoDepVersion(path: string, dep: string): string {
  const text = readFileSync(join(repoRoot, path), "utf8");
  const m = new RegExp(
    `^${dep} = \\{ path = "[^"]+", version = "([^"]+)" \\}`,
    "m",
  ).exec(text);
  if (!m) fail(`could not parse ${dep} version pin from ${path}`);
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

// The workspace pin in src-tauri/Cargo.toml must track the sidecar crate:
// cargo accepts a caret range (^0.2.4 would silently build against 0.2.5),
// so only an explicit assertion catches the drift.
const sidecarPin = cargoDepVersion("src-tauri/Cargo.toml", "dsh-sidecar");
if (sidecarPin !== sidecarCargo) {
  fail(`dsh-sidecar pin drift: src-tauri/Cargo.toml pins ${sidecarPin}, crate is ${sidecarCargo}`);
}
ok(`dsh-sidecar pin aligned: ${sidecarPin}`);

const harnessPin = runtimePkg.dependencies?.["@deepseek-ai/dsh"];
if (harnessPin !== manifest.harnessVersion) {
  fail(`harness pin drift: runtime/package.json pins ${harnessPin}, manifest says ${manifest.harnessVersion}`);
}
ok(`harness pin aligned: ${harnessPin}`);

// The lockfile's RESOLVED version must match too: npm ci installs what the
// lock resolves, so a pin/lock mismatch would ship silently unless the
// prepare step catches it — deny here, at the earliest line of defense.
const runtimeLock = readJson("runtime/package-lock.json") as {
  packages?: Record<string, { version?: string }>;
};
const lockedDsh = runtimeLock.packages?.["node_modules/@deepseek-ai/dsh"]?.version;
if (lockedDsh !== manifest.harnessVersion) {
  fail(
    `harness lockfile drift: package-lock resolves @deepseek-ai/dsh@${lockedDsh}, ` +
      `manifest says ${manifest.harnessVersion} (refresh with npm install in runtime/)`,
  );
}
ok(`harness lockfile aligned: ${lockedDsh}`);

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

// --- deny: npm version (allowlist precondition, both bounds) ---------------
// Shared with prepare-harness so the two pipelines cannot diverge (a local
// `pnpm runtime:all` on an unaudited npm major would otherwise silently run
// install scripts unreviewed). DSH_ALLOW_NPM_MAJOR is the reviewed override.
const npmVersion = assertNpmInAuditedRange();
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
