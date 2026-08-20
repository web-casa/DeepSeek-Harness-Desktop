// Build a standalone Flatpak bundle from the already materialized Tauri DEB.
// The build has no network sources: it imports only the exact local DEB and
// committed AppStream metadata. Runtime/SDK acquisition remains an explicit
// workflow step from the configured Flathub remote.
//
// Deliberately use Flatpak's stable build-init/build-finish/build-export
// primitives instead of flatpak-builder. Ubuntu 22.04 ships flatpak-builder
// 1.2, whose cleanup phase unconditionally invokes the retired
// `appstream-compose` helper inside modern GNOME SDK sandboxes. The low-level
// commands produce the same app commit without that version-sensitive hook.

import {
  copyFileSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { basename, join } from "node:path";
import { repoRoot, tmpDir, fail, info, ok, readManifest } from "./lib/common.ts";
import { bundleArtifactCandidates, type ReleaseArch } from "./lib/release-artifacts.ts";
import {
  FLATPAK_BRANCH,
  FLATPAK_COMMAND,
  FLATPAK_ID,
  FLATPAK_FINISH_ARGS,
  FLATPAK_RUNTIME_REPO,
  FLATPAK_RUNTIME_VERSION,
  flatpakArch,
  flatpakContractProblems,
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

const arch = requestedArch();
const problems = flatpakContractProblems();
if (problems.length > 0) fail(problems.join("\n"));

const tauriConfig = JSON.parse(
  readFileSync(join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"),
) as { identifier?: string; productName?: string };
if (tauriConfig.identifier !== FLATPAK_ID) {
  fail(`Flatpak ID ${FLATPAK_ID} must equal Tauri identifier ${tauriConfig.identifier}`);
}
const productName = tauriConfig.productName?.trim();
if (!productName) fail("Tauri productName is required for Flatpak packaging");

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
const buildDirectory = join(scratch, "build");
const debDirectory = join(scratch, "deb");
const repository = join(scratch, "repo");
const metainfoName = `${FLATPAK_ID}.metainfo.xml`;
const metainfoSource = join(repoRoot, "packaging", "flatpak", metainfoName);
const outputDirectory = join(repoRoot, "target", "release", "bundle", "flatpak");
const version = readManifest().desktopVersion;
const output = join(
  outputDirectory,
  `${productName}_${version}_${flatpakArch(arch)}.flatpak`,
);
const metainfoProblems = flatpakMetadataProblems(readFileSync(metainfoSource, "utf8"));
if (metainfoProblems.length > 0) fail(metainfoProblems.join("\n"));

rmSync(scratch, { recursive: true, force: true });
rmSync(outputDirectory, { recursive: true, force: true });
mkdirSync(scratch, { recursive: true });
mkdirSync(outputDirectory, { recursive: true });

info(`building ${FLATPAK_ID} with org.gnome.Platform ${FLATPAK_RUNTIME_VERSION}`);
run("appstreamcli", ["validate", "--no-net", metainfoSource], "AppStream validation");
run(
  "flatpak",
  [
    "build-init",
    `--arch=${flatpakArch(arch)}`,
    buildDirectory,
    FLATPAK_ID,
    "org.gnome.Sdk",
    "org.gnome.Platform",
    FLATPAK_RUNTIME_VERSION,
  ],
  "Flatpak build directory initialization",
);
run("dpkg-deb", ["-x", debs[0], debDirectory], "Flatpak source DEB extraction");

const filesDirectory = join(buildDirectory, "files");
const shareDirectory = join(filesDirectory, "share");
mkdirSync(join(filesDirectory, "bin"), { recursive: true });
mkdirSync(join(filesDirectory, "lib"), { recursive: true });
mkdirSync(join(shareDirectory, "applications"), { recursive: true });
mkdirSync(join(shareDirectory, "icons", "hicolor"), { recursive: true });
mkdirSync(join(shareDirectory, "metainfo"), { recursive: true });
run(
  "cp",
  [
    "--archive",
    join(debDirectory, "usr", "bin", FLATPAK_COMMAND),
    join(filesDirectory, "bin"),
  ],
  "Flatpak main-binary import",
);
run(
  "cp",
  ["--archive", join(debDirectory, "usr", "lib", productName), join(filesDirectory, "lib")],
  "Flatpak runtime import",
);
run(
  "cp",
  [
    "--archive",
    `${join(debDirectory, "usr", "share", "icons", "hicolor")}/.`,
    join(shareDirectory, "icons", "hicolor"),
  ],
  "Flatpak icon import",
);

const desktopSource = join(
  debDirectory,
  "usr",
  "share",
  "applications",
  `${productName}.desktop`,
);
const desktopDestination = join(shareDirectory, "applications", `${FLATPAK_ID}.desktop`);
const desktop = readFileSync(desktopSource, "utf8").replace(
  /^Icon=.*$/m,
  `Icon=${FLATPAK_ID}`,
);
writeFileSync(desktopDestination, desktop);
copyFileSync(metainfoSource, join(shareDirectory, "metainfo", metainfoName));

let renamedIcons = 0;
for (const size of readdirSync(join(shareDirectory, "icons", "hicolor"))) {
  const appsDirectory = join(shareDirectory, "icons", "hicolor", size, "apps");
  const source = join(appsDirectory, `${FLATPAK_COMMAND}.png`);
  try {
    renameSync(source, join(appsDirectory, `${FLATPAK_ID}.png`));
    renamedIcons += 1;
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code !== "ENOENT") throw error;
  }
}
if (renamedIcons === 0) fail("Flatpak source DEB did not contain an application icon");

run(
  "flatpak",
  [
    "build-finish",
    `--command=${FLATPAK_COMMAND}`,
    ...FLATPAK_FINISH_ARGS,
    buildDirectory,
  ],
  "Flatpak metadata finalization",
);
run(
  "flatpak",
  [
    "build-export",
    `--arch=${flatpakArch(arch)}`,
    repository,
    buildDirectory,
    FLATPAK_BRANCH,
  ],
  "Flatpak repository export",
);
run(
  "flatpak",
  [
    "build-bundle",
    `--arch=${flatpakArch(arch)}`,
    `--runtime-repo=${FLATPAK_RUNTIME_REPO}`,
    repository,
    output,
    FLATPAK_ID,
    FLATPAK_BRANCH,
  ],
  "Flatpak bundle creation",
);
ok(`Flatpak bundle ready: ${output}`);
