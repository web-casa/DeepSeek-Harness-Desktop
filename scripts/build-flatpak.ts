// Build a standalone Flatpak bundle from the already materialized Tauri DEB.
// The module has no network sources: flatpak-builder imports only the exact
// local DEB and committed AppStream metadata. Runtime/SDK acquisition remains
// an explicit workflow step from the configured Flathub remote.

import {
  copyFileSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { basename, join } from "node:path";
import { repoRoot, tmpDir, fail, info, ok, readManifest } from "./lib/common.ts";
import { bundleArtifactCandidates, type ReleaseArch } from "./lib/release-artifacts.ts";
import {
  FLATPAK_ID,
  FLATPAK_RUNTIME_VERSION,
  flatpakArch,
  flatpakContractProblems,
  flatpakManifest,
  flatpakMetadataProblems,
} from "./lib/flatpak.ts";

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function requestedArch(): ReleaseArch {
  const value = argument("--arch") ?? process.arch;
  if (value !== "x64" && value !== "arm64") fail("--arch must be x64 or arm64");
  if (value !== process.arch) {
    fail(`Flatpak must be built natively: requested ${value}, host is ${process.arch}`);
  }
  return value;
}

function run(command: string, args: string[], what: string): string {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    fail(
      `${what} failed (exit ${result.status}, ${result.error?.message ?? "no spawn error"}):\n${(result.stderr ?? result.stdout ?? "").trim().slice(-5000)}`,
    );
  }
  return result.stdout ?? "";
}

function runVisible(command: string, args: string[], what: string): void {
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.status !== 0) {
    fail(`${what} failed (exit ${result.status}, ${result.error?.message ?? "no spawn error"})`);
  }
}

const arch = requestedArch();
const problems = flatpakContractProblems();
if (problems.length > 0) fail(problems.join("\n"));

const tauriConfig = JSON.parse(
  readFileSync(join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"),
) as { identifier?: string };
if (tauriConfig.identifier !== FLATPAK_ID) {
  fail(`Flatpak ID ${FLATPAK_ID} must equal Tauri identifier ${tauriConfig.identifier}`);
}

const debs = bundleArtifactCandidates(repoRoot, "deb");
if (debs.length !== 1) {
  fail(`expected exactly one DEB input, found ${debs.map((path) => basename(path)).join(", ") || "none"}`);
}
const debArch = run("dpkg-deb", ["-f", debs[0], "Architecture"], "DEB architecture query").trim();
const expectedDebArch = arch === "x64" ? "amd64" : "arm64";
if (debArch !== expectedDebArch) {
  fail(`DEB architecture ${debArch} does not match ${expectedDebArch}`);
}

const scratch = join(tmpDir, `flatpak-build-${arch}`);
const sourceDirectory = join(scratch, "sources");
const buildDirectory = join(scratch, "build");
const repository = join(scratch, "repo");
const manifestPath = join(sourceDirectory, "manifest.json");
const metainfoName = `${FLATPAK_ID}.metainfo.xml`;
const metainfoSource = join(repoRoot, "packaging", "flatpak", metainfoName);
const outputDirectory = join(repoRoot, "target", "release", "bundle", "flatpak");
const version = readManifest().desktopVersion;
const output = join(
  outputDirectory,
  `DSH Desktop_${version}_${flatpakArch(arch)}.flatpak`,
);
const metainfoProblems = flatpakMetadataProblems(readFileSync(metainfoSource, "utf8"));
if (metainfoProblems.length > 0) fail(metainfoProblems.join("\n"));

rmSync(scratch, { recursive: true, force: true });
rmSync(outputDirectory, { recursive: true, force: true });
mkdirSync(sourceDirectory, { recursive: true });
mkdirSync(outputDirectory, { recursive: true });
copyFileSync(debs[0], join(sourceDirectory, "app.deb"));
copyFileSync(metainfoSource, join(sourceDirectory, metainfoName));
writeFileSync(manifestPath, `${JSON.stringify(flatpakManifest(), null, 2)}\n`);

info(`building ${FLATPAK_ID} with org.gnome.Platform ${FLATPAK_RUNTIME_VERSION}`);
run("appstreamcli", ["validate", "--no-net", metainfoSource], "AppStream validation");
runVisible(
  "flatpak-builder",
  [
    "--user",
    "--force-clean",
    "--disable-rofiles-fuse",
    `--state-dir=${join(scratch, "state")}`,
    `--repo=${repository}`,
    buildDirectory,
    manifestPath,
  ],
  "flatpak-builder",
);
runVisible(
  "flatpak",
  [
    "build-bundle",
    `--arch=${flatpakArch(arch)}`,
    repository,
    output,
    FLATPAK_ID,
    "stable",
  ],
  "Flatpak bundle export",
);
ok(`Flatpak bundle ready: ${output}`);
