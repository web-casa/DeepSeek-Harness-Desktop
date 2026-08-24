// Verify a built Snap rather than trusting Snapcraft's successful exit status.
// The package is un-squashed into a repo-local temporary directory and checked
// for confinement, architecture, URI routing, launcher, and runtime contents.

import { spawnSync, type SpawnSyncOptionsWithStringEncoding } from "node:child_process";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { basename, join, resolve } from "node:path";
import { fail, ok, readManifest, repoRoot } from "./lib/common.ts";
import {
  SNAP_DESKTOP_ENTRY,
  SNAP_DESKTOP_LAUNCHER,
  SNAP_COMMAND_CHAIN_RUNNER,
  SNAP_GPU_WRAPPER,
  SNAP_LAUNCHER,
  snapTargetForArch,
} from "./lib/snap.ts";
import { sha256File, snapMetadataProblems, snapProvenanceProblems } from "./lib/snap-package.ts";
import type { ReleaseArch } from "./lib/release-artifacts.ts";

interface Options {
  arch: ReleaseArch;
  snap: string;
  provenance?: string;
}

function parseOptions(): Options {
  let arch: ReleaseArch | undefined;
  let snap: string | undefined;
  let provenance: string | undefined;
  for (let index = 2; index < process.argv.length; index += 1) {
    const argument = process.argv[index];
    if (argument !== "--arch" && argument !== "--snap" && argument !== "--provenance") {
      fail(`unknown argument: ${argument}`);
    }
    const value = process.argv[++index];
    if (!value) fail(`${argument} requires a value`);
    if (argument === "--arch") {
      if (arch !== undefined || (value !== "x64" && value !== "arm64")) {
        fail("--arch must appear once and be x64 or arm64");
      }
      arch = value;
    } else if (argument === "--snap") {
      if (snap !== undefined) fail("--snap may appear only once");
      snap = value;
    } else {
      if (provenance !== undefined) fail("--provenance may appear only once");
      provenance = value;
    }
  }
  if (!arch || !snap) {
    fail("usage: node scripts/verify-snap.ts --arch <x64|arm64> --snap <package.snap> [--provenance <json>]");
  }
  return { arch, snap: resolve(repoRoot, snap), provenance: provenance && resolve(repoRoot, provenance) };
}

function run(
  command: string,
  args: string[],
  what: string,
  options: Partial<SpawnSyncOptionsWithStringEncoding> = {},
): string {
  const result = spawnSync(command, args, {
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

function assertExecutable(path: string, label: string): void {
  assertRegularFile(path, label);
  if ((lstatSync(path).mode & 0o111) === 0) {
    throw new Error(`${label} is not executable`);
  }
}

function assertElf(path: string, arch: ReleaseArch, label: string): void {
  const output = run("file", [path], `${label} architecture`).toLowerCase();
  const archMarkers = arch === "x64" ? ["x86-64", "x86_64"] : ["aarch64", "arm aarch64"];
  if (!output.includes("elf") || !archMarkers.some((marker) => output.includes(marker))) {
    throw new Error(`${label} must be ELF ${arch}, got ${output.trim()}`);
  }
}

function countSymlinks(root: string): number {
  let links = 0;
  const visit = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) links += 1;
      else if (stat.isDirectory()) visit(path);
    }
  };
  visit(root);
  return links;
}

function assertScratchRoot(path: string): void {
  if (existsSync(path)) {
    const stat = lstatSync(path);
    if (!stat.isDirectory() || stat.isSymbolicLink()) {
      throw new Error(`Snap verifier scratch root is unsafe: ${path}`);
    }
  } else {
    mkdirSync(path, { recursive: true, mode: 0o700 });
  }
}

function checkBundledManifest(path: string): void {
  const bundled = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
  const source = readManifest();
  for (const key of ["desktopVersion", "nodeVersion", "harnessVersion", "sidecarVersion"] as const) {
    if (bundled[key] !== source[key]) {
      throw new Error(`bundled runtime manifest ${key}=${String(bundled[key])} != ${source[key]}`);
    }
  }
}

function currentSourceCommit(): string {
  const commit = run("git", ["rev-parse", "--verify", "HEAD"], "Git revision query").trim();
  if (!/^[0-9a-f]{40}$/i.test(commit)) {
    throw new Error(`Git revision is not a full SHA: ${commit}`);
  }
  return commit;
}

function verify(options: Options): void {
  const target = snapTargetForArch(options.arch);
  assertRegularFile(options.snap, "Snap artifact");
  if (!options.snap.endsWith(".snap")) fail(`Snap artifact must end in .snap: ${basename(options.snap)}`);
  if (options.provenance) assertRegularFile(options.provenance, "Snap provenance");

  const scratchRoot = join(repoRoot, "target", "snap", "verify");
  assertScratchRoot(scratchRoot);
  const extracted = mkdtempSync(join(scratchRoot, `${target.arch}-`));
  try {
    run("unsquashfs", ["-no-progress", "-d", extracted, options.snap], "Snap extraction");

    const metadata = readFileSync(join(extracted, "meta", "snap.yaml"), "utf8");
    const metadataProblems = snapMetadataProblems(metadata, {
      version: readManifest().desktopVersion,
      architecture: target.snapArchitecture,
    });
    if (metadataProblems.length > 0) {
      throw new Error(`Snap metadata contract drift:\n- ${metadataProblems.join("\n- ")}`);
    }

    const desktop = join(extracted, "meta", "gui", "dsh-desktop-community.desktop");
    assertRegularFile(desktop, "Snap desktop entry");
    if (readFileSync(desktop, "utf8") !== SNAP_DESKTOP_ENTRY) {
      throw new Error("built Snap desktop entry diverges from the reviewed dsharness URI contract");
    }
    const icon = join(extracted, "meta", "gui", "dsh-desktop-community.png");
    assertRegularFile(icon, "Snap Store icon");
    const iconBytes = readFileSync(icon);
    if (
      iconBytes.length < 8 ||
      !iconBytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))
    ) {
      throw new Error("Snap Store icon is not a PNG file");
    }
    const launcher = join(extracted, "bin", "launch-dsh-desktop");
    assertExecutable(launcher, "Snap launcher");
    if (readFileSync(launcher, "utf8") !== SNAP_LAUNCHER) {
      throw new Error("built Snap launcher diverges from the reviewed persistent-data/runtime contract");
    }
    const gpuWrapper = join(extracted, "snap", "command-chain", "gpu-2404-wrapper");
    const desktopLauncher = join(extracted, "snap", "command-chain", "desktop-launch");
    const commandChainRunner = join(extracted, "snap", "command-chain", "run");
    assertExecutable(gpuWrapper, "Snap GPU command-chain relay");
    assertExecutable(desktopLauncher, "Snap desktop command-chain relay");
    assertExecutable(commandChainRunner, "Snap command-chain runner");
    if (readFileSync(gpuWrapper, "utf8") !== SNAP_GPU_WRAPPER) {
      throw new Error("built Snap GPU command-chain relay diverges from the reviewed provider contract");
    }
    if (readFileSync(desktopLauncher, "utf8") !== SNAP_DESKTOP_LAUNCHER) {
      throw new Error("built Snap desktop command-chain relay diverges from the reviewed provider contract");
    }
    if (readFileSync(commandChainRunner, "utf8") !== SNAP_COMMAND_CHAIN_RUNNER) {
      throw new Error("built Snap command-chain runner diverges from the reviewed provider contract");
    }

    const runtime = join(extracted, "usr", "lib", "DSH Desktop", "runtime");
    const main = join(extracted, "usr", "bin", "deepseek-harness-desktop");
    const node = join(runtime, "node");
    const sidecar = join(runtime, "sidecar");
    const harness = join(runtime, "harness");
    const pty = join(harness, "node_modules", "node-pty", "prebuilds", `linux-${target.arch}`, "pty.node");
    for (const [path, label] of [
      [main, "main binary"],
      [node, "bundled Node"],
      [sidecar, "sidecar"],
      [pty, "node-pty native addon"],
      [join(harness, "package.json"), "Harness package.json"],
      [join(harness, "runtime-manifest.json"), "Harness runtime manifest"],
      [join(harness, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js"), "Harness CLI"],
    ] as const) {
      if (label.endsWith("binary") || label === "bundled Node" || label === "sidecar") {
        assertExecutable(path, label);
        assertElf(path, target.arch, label);
      } else {
        assertRegularFile(path, label);
      }
    }
    assertElf(pty, target.arch, "node-pty native addon");
    const links = countSymlinks(harness);
    if (links !== 0) throw new Error(`Snap Harness tree contains ${links} symlink(s)`);
    checkBundledManifest(join(harness, "runtime-manifest.json"));

    if (options.provenance) {
      const provenance = JSON.parse(readFileSync(options.provenance, "utf8")) as unknown;
      const problems = snapProvenanceProblems(provenance, {
        version: readManifest().desktopVersion,
        arch: target.arch,
        snapArchitecture: target.snapArchitecture,
        snapSha256: sha256File(options.snap),
        sourceCommit: currentSourceCommit(),
      });
      if (problems.length > 0) {
        throw new Error(`Snap provenance contract drift:\n- ${problems.join("\n- ")}`);
      }
    }
  } finally {
    rmSync(extracted, { recursive: true, force: true });
  }
  ok(`Snap ${basename(options.snap)} is strict, native ${target.snapArchitecture}, and runtime-complete`);
}

try {
  verify(parseOptions());
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}
