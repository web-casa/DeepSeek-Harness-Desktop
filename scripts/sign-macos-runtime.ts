// Sign every non-Node Mach-O image in the runtime resource tree before Tauri
// copies it into the application. Apple notarization validates nested native
// code even below Contents/Resources; Tauri only auto-discovers conventional
// MacOS and Frameworks locations, so arbitrary npm native addons need this
// explicit inside-out pass. The pinned Node distribution already carries a
// valid Developer ID signature plus its required JIT/library entitlements;
// preserve that upstream signature instead of replacing those entitlements.

import {
  closeSync,
  openSync,
  readSync,
  readdirSync,
  realpathSync,
  statSync,
} from "node:fs";
import { join, relative, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fail, ok, runtimeDir } from "./lib/common.ts";
import { hasHardenedRuntimeFlag, isMachOMagic } from "./lib/macos-signing.ts";

if (process.platform !== "darwin") fail("macOS runtime signing must run on macOS");

const identity = process.env.APPLE_SIGNING_IDENTITY ?? "";
const keychain = process.env.APPLE_KEYCHAIN_PATH ?? "";
const expectedTeam = process.env.APPLE_TEAM_ID ?? "";
if (!identity.startsWith("Developer ID Application:") || !keychain || !expectedTeam) {
  fail("APPLE_SIGNING_IDENTITY, APPLE_KEYCHAIN_PATH and APPLE_TEAM_ID are required");
}

function runCodesign(args: string[], what: string): string {
  const result = spawnSync("codesign", args, { encoding: "utf8" });
  if (result.status !== 0 || result.error) {
    const diagnostic = [result.stdout, result.stderr].filter(Boolean).join("\n").trim();
    fail(`${what} failed${diagnostic ? `: ${diagnostic}` : ""}`);
  }
  return [result.stdout, result.stderr].filter(Boolean).join("\n");
}

function isMachO(path: string): boolean {
  const fd = openSync(path, "r");
  try {
    const header = Buffer.alloc(4);
    const bytes = readSync(fd, header, 0, header.byteLength, 0);
    return bytes === 4 && isMachOMagic(header);
  } finally {
    closeSync(fd);
  }
}

function collectFiles(root: string): string[] {
  const files: string[] = [];
  const pending = [root];
  while (pending.length > 0) {
    const directory = pending.pop();
    if (!directory) break;
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) fail(`runtime symlink found during signing: ${path}`);
      if (entry.isDirectory()) pending.push(path);
      else if (entry.isFile()) files.push(path);
      else fail(`unsupported runtime entry found during signing: ${path}`);
    }
  }
  return files;
}

const root = realpathSync(runtimeDir);
const targets = collectFiles(root).filter(isMachO).sort((a, b) => b.length - a.length);
if (targets.length === 0) fail("no Mach-O files found in staged macOS runtime");
const upstreamNode = resolve(join(root, "node"));
let signedCount = 0;

for (const target of targets) {
  const isUpstreamNode = resolve(target) === upstreamNode;
  if (!isUpstreamNode) {
    runCodesign(
      [
        "--force",
        "--sign",
        identity,
        "--keychain",
        keychain,
        "--options",
        "runtime",
        "--timestamp",
        target,
      ],
      `sign ${relative(root, target)}`,
    );
    signedCount += 1;
  }
  runCodesign(["--verify", "--strict", "--verbose=2", target], `verify ${relative(root, target)}`);
  const details = runCodesign(["--display", "--verbose=4", target], `inspect ${relative(root, target)}`);
  if (!details.includes("Authority=Developer ID Application:")) {
    fail(`Developer ID authority missing after signing ${relative(root, target)}`);
  }
  if (!isUpstreamNode && !details.includes(`TeamIdentifier=${expectedTeam}`)) {
    fail(`TeamIdentifier differs after signing ${relative(root, target)}`);
  }
  if (!/^Timestamp=.+$/m.test(details)) {
    fail(`secure timestamp missing after signing ${relative(root, target)}`);
  }
  if (!hasHardenedRuntimeFlag(details)) {
    fail(`hardened runtime flag missing after signing ${relative(root, target)}`);
  }
}

const signed = new Set(targets.map((path) => resolve(path)));
for (const required of [join(root, "node"), join(root, "sidecar")]) {
  if (!statSync(required).isFile() || !signed.has(resolve(required))) {
    fail(`required runtime executable was not recognized as Mach-O: ${required}`);
  }
}

ok(
  `verified ${targets.length} nested Mach-O runtime files; signed ${signedCount} and preserved the pinned Node Developer ID signature`,
);
