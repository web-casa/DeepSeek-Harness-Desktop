// Validate the checked-in Snap recipe before a package build can consume it.

import { closeSync, constants, fstatSync, lstatSync, openSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fail, ok, readManifest, repoRoot } from "./lib/common.ts";
import { snapDefinitionProblems } from "./lib/snap.ts";

if (process.argv.length !== 2) {
  fail("usage: node scripts/verify-snap-definition.ts");
}

const problems = snapDefinitionProblems({
  recipe: readFileSync(join(repoRoot, "snap", "snapcraft.yaml"), "utf8"),
  launcher: readFileSync(join(repoRoot, "snap", "bin", "launch-dsh-desktop"), "utf8"),
  desktopEntry: readFileSync(
    join(repoRoot, "snap", "gui", "dsh-desktop-community.desktop"),
    "utf8",
  ),
  gpuWrapper: readFileSync(join(repoRoot, "snap", "command-chain", "gpu-2404-wrapper"), "utf8"),
  desktopLauncher: readFileSync(join(repoRoot, "snap", "command-chain", "desktop-launch"), "utf8"),
  commandChainRunner: readFileSync(join(repoRoot, "snap", "command-chain", "run"), "utf8"),
  expectedVersion: readManifest().desktopVersion,
});
const iconPath = join(repoRoot, "snap", "gui", "dsh-desktop-community.png");
try {
  // Open once with no-follow and validate/read through that descriptor. A
  // separate lstat→readFile path would let a replaced checkout entry turn the
  // definition check into a time-of-check/time-of-use race.
  const descriptor = openSync(iconPath, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const icon = fstatSync(descriptor);
    if (!icon.isFile()) {
      problems.push("Snap Store icon must be a regular file");
    } else {
      const bytes = readFileSync(descriptor);
      if (
        bytes.length < 8 ||
        !bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))
      ) {
        problems.push("Snap Store icon must be a PNG file");
      }
    }
  } finally {
    closeSync(descriptor);
  }
} catch {
  problems.push("Snap Store icon is missing, not a regular file, or cannot be securely opened");
}
for (const [path, label] of [
  [join(repoRoot, "snap", "bin", "launch-dsh-desktop"), "Snap launcher"],
  [join(repoRoot, "snap", "command-chain", "gpu-2404-wrapper"), "Snap GPU command-chain relay"],
  [join(repoRoot, "snap", "command-chain", "desktop-launch"), "Snap desktop command-chain relay"],
  [join(repoRoot, "snap", "command-chain", "run"), "Snap command-chain runner"],
] as const) {
  try {
    const script = lstatSync(path);
    if (!script.isFile() || (script.mode & 0o111) === 0) {
      problems.push(`${label} must be a regular executable file`);
    }
  } catch {
    problems.push(`${label} is missing`);
  }
}
if (problems.length > 0) {
  fail(`Snap definition contract drift:\n- ${problems.join("\n- ")}`);
}
ok("Snap recipe, launcher, strict confinement, and dsharness URI contract aligned");
