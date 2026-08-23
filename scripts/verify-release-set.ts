// Run the complete content/signing/checksum contract for one native matrix row.

import { spawnSync } from "node:child_process";
import { statSync } from "node:fs";
import { basename, join } from "node:path";
import { repoRoot, fail, ok } from "./lib/common.ts";
import {
  BUNDLE_SPECS,
  bundleArtifactCandidates,
  publicArtifactCandidates,
  publicArtifactsFor,
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

const artifacts = publicArtifactsFor(target);
for (const bundle of target.bundles) {
  const expected = artifacts.filter((artifact) => artifact.bundle === bundle);
  const resolved = expected.flatMap((artifact) => {
    const candidates = publicArtifactCandidates(repoRoot, artifact);
    if (candidates.length !== 1) {
      fail(
        `${target.id} expected one ${bundle}${artifact.installerLocale ? ` (${artifact.installerLocale})` : ""} artifact, found: ${candidates.map((path) => basename(path)).join(", ") || "none"}`,
      );
    }
    return candidates;
  });
  const actual = bundleArtifactCandidates(repoRoot, bundle);
  const expectedPaths = new Set(resolved);
  if (actual.length !== resolved.length || actual.some((path) => !expectedPaths.has(path))) {
    fail(
      `${target.id} ${bundle} artifact set is not exact: expected ${resolved.map((path) => basename(path)).join(", ") || "none"}, found ${actual.map((path) => basename(path)).join(", ") || "none"}`,
    );
  }
}

for (const artifact of artifacts) {
  const args = ["--bundle", artifact.bundle, "--arch", target.arch];
  if (artifact.installerLocale) args.push("--installer-locale", artifact.installerLocale);
  runScript("verify-bundle.ts", args);
  if (BUNDLE_SPECS[artifact.bundle].signing !== "checksum") {
    runScript("verify-signing.ts", [
      "--bundle",
      artifact.bundle,
      ...(artifact.installerLocale ? ["--installer-locale", artifact.installerLocale] : []),
    ]);
  }
  runScript("checksums.ts", [
    "--bundle",
    artifact.bundle,
    ...(artifact.installerLocale ? ["--installer-locale", artifact.installerLocale] : []),
  ]);
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
