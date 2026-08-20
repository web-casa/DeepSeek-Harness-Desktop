// Run the complete content/signing/checksum contract for one native matrix row.

import { spawnSync } from "node:child_process";
import { statSync } from "node:fs";
import { join } from "node:path";
import { repoRoot, fail, ok } from "./lib/common.ts";
import {
  BUNDLE_SPECS,
  targetById,
  updaterSignatureCandidates,
} from "./lib/release-artifacts.ts";

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function runScript(script: string, args: string[]): void {
  const result = spawnSync(process.execPath, [join(repoRoot, "scripts", script), ...args], {
    stdio: "inherit",
    env: process.env,
  });
  if (result.status !== 0) {
    fail(`${script} ${args.join(" ")} exited with ${result.status}`);
  }
}

const targetId = argument("--target");
if (!targetId) fail("usage: node scripts/verify-release-set.ts --target <matrix-target>");
const target = targetById(targetId);
if (!target) fail(`unknown native release target: ${targetId}`);

const platform =
  process.platform === "win32" ? "windows" : process.platform === "darwin" ? "macos" : "linux";
if (!target.id.startsWith(`${platform}-`)) {
  fail(`target ${target.id} cannot be verified on ${process.platform}`);
}
if (process.arch !== target.arch) {
  fail(`target ${target.id} requires ${target.arch}, current process is ${process.arch}`);
}

for (const bundle of target.bundles) {
  runScript("verify-bundle.ts", ["--bundle", bundle, "--arch", target.arch]);
  if (BUNDLE_SPECS[bundle].signing !== "checksum") {
    runScript("verify-signing.ts", ["--bundle", bundle]);
  }
  runScript("checksums.ts", ["--bundle", bundle]);
}
if (target.updaterSignature) {
  const signatures = updaterSignatureCandidates(repoRoot, target);
  if (signatures.length !== 1 || statSync(signatures[0]).size === 0) {
    fail(
      `${target.id} expected one non-empty updater signature, found ${signatures.join(", ") || "none"}`,
    );
  }
  ok(
    `${target.id} updater signature exists (${target.updaterSignature.publish ? "published" : "build-only tripwire"})`,
  );
}
ok(`release set verified for ${target.id}: ${target.bundles.join(", ")}`);
