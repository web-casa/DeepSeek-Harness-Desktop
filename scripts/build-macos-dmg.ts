// Build a compressed macOS disk image without mounting it or driving Finder.
// Tauri's styled DMG helper depends on attach/AppleScript/detach, which can
// leave diskimages-helper blocked on hosted Intel runners. hdiutil's direct
// -srcfolder mode keeps the same signed-app + Applications-link contract with
// a bounded, non-interactive command chain.

import { spawnSync } from "node:child_process";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { fail, info, ok, readManifest, repoRoot } from "./lib/common.ts";
import {
  DMG_CREATE_TIMEOUT_MS,
  DMG_TOOL_TIMEOUT_MS,
  macosDmgPaths,
} from "./lib/macos-dmg.ts";
import type { ReleaseArch } from "./lib/release-artifacts.ts";

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function requestedArch(): ReleaseArch {
  const value = argument("--arch") ?? process.arch;
  if (value !== "x64" && value !== "arm64") fail("--arch must be x64 or arm64");
  if (value !== process.arch) {
    fail(`DMG must be built natively: requested ${value}, host is ${process.arch}`);
  }
  return value;
}

function commandOutput(...values: readonly (string | null | undefined)[]): string {
  return values
    .map((value) => value?.trim() ?? "")
    .filter(Boolean)
    .join("\n")
    .slice(-5000);
}

function run(command: string, args: string[], label: string, timeout: number): void {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout,
    killSignal: "SIGKILL",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    const timeoutHint = result.error?.message.includes("ETIMEDOUT")
      ? ` after ${Math.ceil(timeout / 60000)} minutes`
      : "";
    throw new Error(
      `${label} failed${timeoutHint} (exit ${result.status}, ${result.error?.message ?? "no spawn error"}):\n` +
        commandOutput(result.stdout, result.stderr),
    );
  }
}

if (process.platform !== "darwin") fail("DMG packaging requires macOS");

const arch = requestedArch();
const tauriConfig = JSON.parse(
  readFileSync(join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"),
) as { productName?: string };
const productName = tauriConfig.productName?.trim();
if (!productName) fail("Tauri productName is required for DMG packaging");

const signingConfigured = process.env.APPLE_SIGNING_CONFIGURED;
const signingIdentity = process.env.APPLE_SIGNING_IDENTITY?.trim();
if (signingConfigured !== "0" && signingConfigured !== "1") {
  fail("APPLE_SIGNING_CONFIGURED must be 0 or 1");
}
if (signingConfigured === "1" && !signingIdentity) {
  fail("APPLE_SIGNING_IDENTITY is required for a signed DMG");
}
if (signingConfigured === "0" && signingIdentity) {
  fail("unsigned DMG configuration unexpectedly contains a signing identity");
}

const paths = macosDmgPaths(repoRoot, productName, readManifest().desktopVersion, arch);
if (!existsSync(paths.appDirectory)) fail("macOS app bundle directory does not exist");
const appCandidates = readdirSync(paths.appDirectory)
  .filter((name) => name.endsWith(".app"))
  .map((name) => join(paths.appDirectory, name));
if (appCandidates.length !== 1) {
  fail(
    `expected exactly one app bundle, found ${appCandidates.map((path) => basename(path)).join(", ") || "none"}`,
  );
}
const appMetadata = lstatSync(appCandidates[0]);
if (appMetadata.isSymbolicLink() || !appMetadata.isDirectory()) {
  fail("macOS app bundle must be a real directory, not a symlink");
}

rmSync(paths.outputDirectory, { recursive: true, force: true });
mkdirSync(paths.outputDirectory, { recursive: true });
const stagingDirectory = mkdtempSync(join(tmpdir(), "dsh-dmg-"));
let buildError: unknown;

try {
  const stagedApp = join(stagingDirectory, basename(appCandidates[0]));
  info(`staging ${basename(appCandidates[0])} without Finder automation`);
  run(
    "ditto",
    ["--rsrc", "--extattr", "--acl", appCandidates[0], stagedApp],
    "app staging",
    DMG_TOOL_TIMEOUT_MS,
  );
  symlinkSync("/Applications", join(stagingDirectory, "Applications"));

  run(
    "hdiutil",
    [
      "create",
      "-volname",
      productName,
      "-srcfolder",
      stagingDirectory,
      "-format",
      "UDZO",
      "-imagekey",
      "zlib-level=9",
      "-ov",
      paths.output,
    ],
    "direct DMG creation",
    DMG_CREATE_TIMEOUT_MS,
  );

  if (signingIdentity) {
    run(
      "codesign",
      ["--force", "--sign", signingIdentity, "--timestamp", paths.output],
      "DMG signing",
      DMG_TOOL_TIMEOUT_MS,
    );
  }
  run("hdiutil", ["verify", paths.output], "DMG checksum verification", DMG_TOOL_TIMEOUT_MS);

  const outputMetadata = lstatSync(paths.output);
  if (outputMetadata.isSymbolicLink() || !outputMetadata.isFile() || outputMetadata.size === 0) {
    throw new Error("DMG output must be a non-empty regular file");
  }
} catch (error) {
  buildError = error;
} finally {
  try {
    rmSync(stagingDirectory, { recursive: true, force: true });
  } catch (error) {
    console.warn(
      `warning: failed to remove temporary DMG staging directory: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
}

if (buildError) {
  rmSync(paths.output, { force: true });
  fail(buildError instanceof Error ? buildError.message : String(buildError));
}

ok(`built ${basename(paths.output)} (${signingIdentity ? "Developer ID signed" : "unsigned"})`);
