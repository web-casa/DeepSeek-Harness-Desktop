// Build a strict Snap from the current job's source-built, already verified
// Debian package. This never accepts a GitHub Release download as its input.

import { spawnSync, type SpawnSyncOptionsWithStringEncoding } from "node:child_process";
import { copyFileSync, existsSync, lstatSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";
import { fail, ok, readManifest, repoRoot } from "./lib/common.ts";
import { bundleArtifactCandidates } from "./lib/release-artifacts.ts";
import { SNAPCRAFT_VERSION, snapTargetForArch } from "./lib/snap.ts";
import { sha256File, snapArtifactName, type SnapProvenance } from "./lib/snap-package.ts";
import type { ReleaseArch } from "./lib/release-artifacts.ts";

function parseArch(): ReleaseArch {
  if (process.argv.length !== 4 || process.argv[2] !== "--arch") {
    fail("usage: node scripts/build-snap.ts --arch <x64|arm64>");
  }
  const arch = process.argv[3];
  if (arch !== "x64" && arch !== "arm64") fail("--arch must be x64 or arm64");
  return arch;
}

function run(
  command: string,
  args: string[],
  what: string,
  options: Partial<SpawnSyncOptionsWithStringEncoding> = {},
): string {
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    ...options,
  });
  if (result.status !== 0) {
    throw new Error(
      `${what} failed (exit ${result.status}, ${result.error?.message ?? "no spawn error"}):\n${(result.stderr || result.stdout || "no output").trim().slice(-8000)}`,
    );
  }
  return result.stdout ?? "";
}

function assertRegularFile(path: string, label: string): void {
  if (!existsSync(path)) throw new Error(`${label} is missing: ${path}`);
  const stat = lstatSync(path);
  if (!stat.isFile()) throw new Error(`${label} must be a regular file: ${path}`);
}

function removeExistingRegularFile(path: string): void {
  if (!existsSync(path)) return;
  assertRegularFile(path, "existing generated Snap artifact");
  rmSync(path, { force: true });
}

function exactDeb(arch: ReleaseArch): string {
  const candidates = bundleArtifactCandidates(repoRoot, "deb");
  if (candidates.length !== 1) {
    throw new Error(
      `expected exactly one source-built DEB, found ${candidates.map((path) => basename(path)).join(",") || "none"}`,
    );
  }
  const artifact = candidates[0];
  assertRegularFile(artifact, "source-built DEB");
  const debArch = run("dpkg-deb", ["--field", artifact, "Architecture"], "DEB architecture query").trim();
  const expected = arch === "x64" ? "amd64" : "arm64";
  if (debArch !== expected) {
    throw new Error(`source-built DEB architecture ${debArch || "missing"} != expected ${expected}`);
  }
  return artifact;
}

function sourceCommit(): string {
  const commit = run("git", ["rev-parse", "--verify", "HEAD"], "Git revision query").trim();
  if (!/^[0-9a-f]{40}$/i.test(commit)) throw new Error(`Git revision is not a full SHA: ${commit}`);
  return commit;
}

function build(arch: ReleaseArch): void {
  const target = snapTargetForArch(arch);
  run(process.execPath, ["scripts/verify-native-host.ts", "--target", target.nativeTarget], "native host validation");
  run(process.execPath, ["scripts/verify-snap-definition.ts"], "Snap source-definition validation");
  // `verify-bundle` proves that this exact local DEB has all runtime pieces,
  // deep-link registration, architecture and the materialization invariant
  // before Snapcraft is allowed to repackage it.
  run(
    process.execPath,
    ["scripts/verify-bundle.ts", "--bundle", "deb", "--arch", arch],
    "source-built DEB verification",
  );

  const actualSnapcraft = run("snapcraft", ["--version"], "Snapcraft version query").trim();
  if (actualSnapcraft !== `snapcraft ${SNAPCRAFT_VERSION}`) {
    throw new Error(
      `Snapcraft ${actualSnapcraft || "missing"} is not the reviewed ${SNAPCRAFT_VERSION}; update the reviewed pin before packaging`,
    );
  }

  const sourceDeb = exactDeb(arch);
  const sourceDebSha256 = sha256File(sourceDeb);
  const stagedInput = join(repoRoot, "target", "snap", "input", "dsh-desktop.deb");
  mkdirSync(join(repoRoot, "target", "snap", "input"), { recursive: true, mode: 0o700 });
  if (existsSync(stagedInput)) assertRegularFile(stagedInput, "existing staged Snap DEB input");
  copyFileSync(sourceDeb, stagedInput);
  if (sha256File(stagedInput) !== sourceDebSha256) {
    throw new Error("staged Snap DEB SHA-256 does not match the source-built DEB");
  }

  const version = readManifest().desktopVersion;
  const outputDirectory = join(repoRoot, "target", "snap", "packages", arch, version);
  mkdirSync(outputDirectory, { recursive: true, mode: 0o700 });
  const snapName = snapArtifactName(version, target.snapArchitecture);
  const snapPath = join(outputDirectory, snapName);
  const provenancePath = join(outputDirectory, "snap-provenance.json");
  removeExistingRegularFile(snapPath);
  removeExistingRegularFile(provenancePath);
  run(
    "snapcraft",
    ["pack", "--destructive-mode", "--platform", target.snapArchitecture, "--output", outputDirectory],
    "Snapcraft package build",
  );
  const packages = readdirSync(outputDirectory)
    .filter((entry) => entry.endsWith(".snap"))
    .sort();
  if (packages.length !== 1 || packages[0] !== snapName) {
    throw new Error(`expected only ${snapName} from Snapcraft, found ${packages.join(",") || "none"}`);
  }
  assertRegularFile(snapPath, "built Snap");
  const provenance: SnapProvenance = {
    schema: 1,
    name: "dsh-desktop-community",
    version,
    arch,
    snapArchitecture: target.snapArchitecture,
    sourceCommit: sourceCommit(),
    sourceDeb: { sha256: sourceDebSha256 },
    snap: { sha256: sha256File(snapPath) },
    snapcraftVersion: actualSnapcraft,
  };
  writeFileSync(provenancePath, `${JSON.stringify(provenance, null, 2)}\n`, { mode: 0o600 });
  run(
    process.execPath,
    ["scripts/verify-snap.ts", "--arch", arch, "--snap", snapPath, "--provenance", provenancePath],
    "built Snap verification",
  );
  ok(`Snap package ready: ${snapPath}`);
}

try {
  build(parseArch());
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}
