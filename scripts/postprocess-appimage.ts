// Repair the Linuxdeploy/AppImage runtime defects that affect newer
// Wayland/Mesa stacks:
//
// 1. The generated AppImage carries build-machine Wayland, GLib/GIO, and
//    nghttp2 libraries. They are ABI-sensitive with host Mesa, GIO modules,
//    and libcurl, so this small evidence-backed family must be resolved from
//    the target desktop instead.
// 2. AppRun always exports GST_PLUGIN_SYSTEM_PATH_1_0. The Tauri config now
//    enables bundleMediaFramework, and this script refuses to publish an image
//    whose exported plugin directory is absent or empty.
//
// Tauri does not currently expose linuxdeploy's --exclude-library through
// tauri.conf.json.  Rebuild only the SquashFS payload, preserving the exact
// Type-2 AppImage runtime prefix.  All mutations occur in a fresh temp dir;
// the public artifact is atomically replaced only after a fresh extraction
// passes the compatibility contract.

import { spawnSync, type SpawnSyncOptionsWithStringEncoding } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  createReadStream,
  createWriteStream,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  renameSync,
  rmSync,
  statSync,
  truncateSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { pipeline } from "node:stream/promises";
import {
  appImageRuntimeProblems,
  stripBundledHostAbiRuntimeLibraries,
} from "./lib/appimage-runtime.ts";
import { fail, info, ok, repoRoot } from "./lib/common.ts";
import { bundleArtifactCandidates, type ReleaseArch } from "./lib/release-artifacts.ts";

const EXPECTED_SQUASHFS_COMPRESSION = "zstd";
const EXPECTED_SQUASHFS_BLOCK_SIZE = "131072";

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function parseArch(value: string | undefined): ReleaseArch {
  if (value === "x64" || value === "arm64") return value;
  fail(`usage: node scripts/postprocess-appimage.ts --arch <x64|arm64> [--artifact <absolute-AppImage-path>]`);
}

function outputTail(...values: readonly (string | null | undefined)[]): string {
  return values
    .map((value) => value?.trim() ?? "")
    .filter(Boolean)
    .join("\n")
    .slice(-8_000);
}

function run(
  command: string,
  args: readonly string[],
  label: string,
  options: Partial<SpawnSyncOptionsWithStringEncoding> = {},
): string {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    ...options,
  });
  if (result.status !== 0) {
    throw new Error(
      `${label} failed (exit ${result.status ?? "unknown"}, ${result.error?.message ?? "no spawn error"}):\n${outputTail(result.stdout, result.stderr)}`,
    );
  }
  return result.stdout ?? "";
}

function regularExecutable(path: string, label: string): void {
  const status = lstatSync(path);
  if (!status.isFile()) throw new Error(`${label} must be a regular file: ${path}`);
  if ((status.mode & 0o111) === 0) {
    throw new Error(`${label} must be executable (mode ${(status.mode & 0o777).toString(8)}): ${path}`);
  }
}

function assertAppImageArchitecture(path: string, arch: ReleaseArch): void {
  const description = run("file", [path], "AppImage architecture inspection").toLowerCase();
  const expected =
    arch === "x64" ? ["x86-64", "x86_64"] : ["arm aarch64", "aarch64", "arm64"];
  if (!description.includes("elf") || !expected.some((marker) => description.includes(marker))) {
    throw new Error(`AppImage architecture is not ${arch}: ${description.trim()}`);
  }
}

function artifactFor(optionalPath: string | undefined): string {
  if (optionalPath !== undefined) {
    if (!isAbsolute(optionalPath)) {
      throw new Error("--artifact must be an absolute path");
    }
    const artifact = resolve(optionalPath);
    regularExecutable(artifact, "--artifact");
    return artifact;
  }
  const candidates = bundleArtifactCandidates(repoRoot, "appimage");
  if (candidates.length !== 1) {
    throw new Error(
      `expected exactly one AppImage artifact, found: ${candidates.map((path) => basename(path)).join(", ") || "none"}`,
    );
  }
  regularExecutable(candidates[0], "AppImage artifact");
  return candidates[0];
}

function appImageOffset(artifact: string): number {
  const raw = run(artifact, ["--appimage-offset"], "AppImage offset query").trim();
  if (!/^\d+$/.test(raw)) throw new Error(`invalid AppImage offset: ${JSON.stringify(raw)}`);
  const offset = Number(raw);
  const size = statSync(artifact).size;
  if (!Number.isSafeInteger(offset) || offset <= 0 || offset >= size) {
    throw new Error(`AppImage offset ${raw} is outside its file size ${size}`);
  }
  return offset;
}

function assertNoEmbeddedSignature(artifact: string): void {
  // Rebuilding only the SquashFS payload invalidates an AppImage-internal
  // signature. Public Linux artifacts are checksum-protected today; fail
  // closed if that policy ever changes instead of silently shipping a broken
  // embedded signature.
  const signature = run(artifact, ["--appimage-signature"], "AppImage signature inspection");
  if (signature.trim() !== "") {
    throw new Error("AppImage has an embedded signature and cannot be post-processed safely");
  }
}

function assertExpectedSquashfs(artifact: string, offset: number): void {
  const summary = run(
    "unsquashfs",
    ["-offset", String(offset), "-s", artifact],
    "AppImage SquashFS inspection",
  );
  if (
    !new RegExp(`^Compression ${EXPECTED_SQUASHFS_COMPRESSION}$`, "m").test(summary) ||
    !new RegExp(`^Block size ${EXPECTED_SQUASHFS_BLOCK_SIZE}$`, "m").test(summary)
  ) {
    throw new Error(
      `unexpected AppImage SquashFS format; expected ${EXPECTED_SQUASHFS_COMPRESSION}/${EXPECTED_SQUASHFS_BLOCK_SIZE}:\n${outputTail(summary)}`,
    );
  }
}

function extract(artifact: string, destination: string, label: string): string {
  run(artifact, ["--appimage-extract"], label, {
    cwd: destination,
    env: { ...process.env, APPIMAGE_EXTRACT_AND_RUN: "1" },
  });
  const root = join(destination, "squashfs-root");
  if (!existsSync(root) || !lstatSync(root).isDirectory()) {
    throw new Error(`${label} did not create a squashfs-root directory`);
  }
  return root;
}

function assertCompatibility(root: string): void {
  const problems = appImageRuntimeProblems(root);
  if (problems.length > 0) throw new Error(problems.join("\n"));
}

async function rebuildPayload(root: string, destination: string): Promise<void> {
  run(
    "mksquashfs",
    [
      root,
      destination,
      "-noappend",
      "-comp",
      EXPECTED_SQUASHFS_COMPRESSION,
      "-b",
      EXPECTED_SQUASHFS_BLOCK_SIZE,
      "-all-root",
      "-mkfs-time",
      "0",
      "-all-time",
      "0",
    ],
    "AppImage SquashFS rebuild",
  );
}

async function combineRuntimeAndPayload(
  original: string,
  offset: number,
  payload: string,
  destination: string,
): Promise<void> {
  copyFileSync(original, destination);
  truncateSync(destination, offset);
  await pipeline(createReadStream(payload), createWriteStream(destination, { flags: "a" }));
  chmodSync(destination, statSync(original).mode & 0o777);
}

async function main(): Promise<void> {
  if (process.platform !== "linux") fail("AppImage post-processing can run only on Linux");
  const arch = parseArch(argument("--arch"));
  const artifact = artifactFor(argument("--artifact"));
  assertAppImageArchitecture(artifact, arch);
  const scratch = mkdtempSync(join(tmpdir(), "dsh-appimage-postprocess-"));
  // Keep the verified replacement on the artifact's filesystem. `renameSync`
  // is then atomic rather than failing with EXDEV when /tmp is a separate
  // mount (as it commonly is on CI runners and developer machines).
  const staging = mkdtempSync(join(dirname(artifact), `.${basename(artifact)}.postprocess-`));
  try {
    assertNoEmbeddedSignature(artifact);
    const offset = appImageOffset(artifact);
    assertExpectedSquashfs(artifact, offset);
    const root = extract(artifact, scratch, "AppImage extraction before compatibility processing");
    const removed = stripBundledHostAbiRuntimeLibraries(root);
    if (removed.length === 0) {
      info("AppImage contains no bundled host-ABI runtime libraries");
    } else {
      info(`removed bundled host-ABI runtime libraries: ${removed.join(", ")}`);
    }
    assertCompatibility(root);

    const payload = join(scratch, "payload.squashfs");
    await rebuildPayload(root, payload);
    const candidate = join(staging, basename(artifact));
    await combineRuntimeAndPayload(artifact, offset, payload, candidate);
    if (appImageOffset(candidate) !== offset) {
      throw new Error("repacked AppImage changed its runtime offset");
    }
    const validation = join(scratch, "validation");
    mkdirSync(validation);
    const validationRoot = extract(candidate, validation, "repacked AppImage extraction");
    assertCompatibility(validationRoot);
    renameSync(candidate, artifact);
    ok(`AppImage compatibility post-processing passed for ${basename(artifact)} (${arch})`);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
    rmSync(staging, { recursive: true, force: true });
  }
}

main().catch((error: unknown) => {
  fail(error instanceof Error ? error.message : String(error));
});
