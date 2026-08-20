// Preseed the exact tool filenames Tauri 2.11.4 expects in its Linux cache.
// Tauri's fallback URLs include moving branches/releases; every byte used by
// this release path is instead fetched from a reviewed asset ID or commit and
// checked against a committed SHA-256 before it becomes visible to Tauri.

import { createHash } from "node:crypto";
import {
  chmodSync,
  createReadStream,
  existsSync,
  mkdirSync,
  renameSync,
  rmSync,
} from "node:fs";
import { open } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import {
  appImageToolDefinitionProblems,
  appImageToolsForArch,
  type AppImageTool,
} from "./lib/appimage-tools.ts";
import { fail, info, ok, repoRoot } from "./lib/common.ts";
import type { ReleaseArch } from "./lib/release-artifacts.ts";

const DOWNLOAD_TIMEOUT_MS = 2 * 60_000;
const DOWNLOAD_RETRIES = 3;
const MAX_TOOL_BYTES = 64 * 1024 * 1024;

function requestedArch(): ReleaseArch {
  const index = process.argv.indexOf("--arch");
  const value = index >= 0 ? process.argv[index + 1] : process.arch;
  if (value !== "x64" && value !== "arm64") {
    fail(`unsupported AppImage tool architecture: ${value ?? "missing"}`);
  }
  return value;
}

function tauriCacheDirectory(): string {
  const xdgCache = process.env.XDG_CACHE_HOME;
  if (xdgCache !== undefined && !isAbsolute(xdgCache)) {
    fail("XDG_CACHE_HOME must be an absolute path for deterministic Tauri tool staging");
  }
  return join(xdgCache ?? join(homedir(), ".cache"), "tauri");
}

async function sha256File(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

function requestHeaders(tool: AppImageTool): Record<string, string> {
  const headers: Record<string, string> = {
    "user-agent": "DeepSeek-Harness-Desktop-release",
  };
  if (new URL(tool.source).hostname === "api.github.com") {
    headers.accept = "application/octet-stream";
    const token = process.env.GITHUB_TOKEN;
    if (token) headers.authorization = `Bearer ${token}`;
  }
  return headers;
}

async function downloadOnce(tool: AppImageTool, destination: string): Promise<void> {
  const response = await fetch(tool.source, {
    redirect: "follow",
    headers: requestHeaders(tool),
    signal: AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS),
  });
  if (!response.ok || response.body === null) {
    throw new Error(`HTTP ${response.status}`);
  }
  const declaredSize = Number(response.headers.get("content-length") ?? "0");
  if (Number.isFinite(declaredSize) && declaredSize > MAX_TOOL_BYTES) {
    throw new Error(`declared size ${declaredSize} exceeds ${MAX_TOOL_BYTES} bytes`);
  }

  const file = await open(destination, "wx", 0o700);
  const hash = createHash("sha256");
  let received = 0;
  try {
    for await (const chunk of response.body) {
      received += chunk.byteLength;
      if (received > MAX_TOOL_BYTES) {
        throw new Error(`download exceeds ${MAX_TOOL_BYTES} bytes`);
      }
      hash.update(chunk);
      // FileHandle.write may legally complete with a short write. Keep the
      // on-disk byte stream identical to the bytes that were hashed instead
      // of assuming a single syscall consumed the whole response chunk.
      let offset = 0;
      while (offset < chunk.byteLength) {
        const { bytesWritten } = await file.write(
          chunk,
          offset,
          chunk.byteLength - offset,
        );
        if (bytesWritten <= 0) throw new Error("download write made no progress");
        offset += bytesWritten;
      }
    }
  } finally {
    await file.close();
  }
  const actual = hash.digest("hex");
  if (actual !== tool.sha256) {
    throw new Error(`SHA-256 mismatch: expected ${tool.sha256}, got ${actual}`);
  }
}

async function stageTool(tool: AppImageTool, cacheDirectory: string): Promise<void> {
  const destination = join(cacheDirectory, tool.cacheName);
  if (existsSync(destination) && (await sha256File(destination)) === tool.sha256) {
    chmodSync(destination, 0o770);
    info(`verified cached ${tool.cacheName}`);
    return;
  }

  // Tauri patches linuxdeploy in place. A mismatched cache is expected on a
  // repeated build, but it can never be reused without restoring pristine,
  // reviewed bytes first.
  const partial = join(
    cacheDirectory,
    `.${tool.cacheName}.download-${process.pid}-${Date.now()}`,
  );
  rmSync(partial, { force: true });
  let lastError: unknown;
  for (let attempt = 1; attempt <= DOWNLOAD_RETRIES; attempt += 1) {
    try {
      info(`downloading ${tool.cacheName} (attempt ${attempt})`);
      await downloadOnce(tool, partial);
      const stagedHash = await sha256File(partial);
      if (stagedHash !== tool.sha256) {
        throw new Error(
          `staged-file SHA-256 mismatch: expected ${tool.sha256}, got ${stagedHash}`,
        );
      }
      chmodSync(partial, 0o770);
      renameSync(partial, destination);
      return;
    } catch (error) {
      lastError = error;
      rmSync(partial, { force: true });
    }
  }
  throw new Error(
    `${tool.cacheName} failed after ${DOWNLOAD_RETRIES} attempts: ${lastError instanceof Error ? lastError.message : String(lastError)}`,
  );
}

async function main(): Promise<void> {
  if (process.platform !== "linux") fail("AppImage tools can only be staged on Linux");
  const arch = requestedArch();
  const problems = appImageToolDefinitionProblems(arch);
  if (problems.length > 0) fail(problems.join("\n"));
  // tauri-bundler removes appimage_deb only after a successful linuxdeploy
  // run. A failed attempt can otherwise leak stale runtime files into a retry
  // even after the reviewed source tree changed.
  const stalePackageDirectory = join(
    repoRoot,
    "target",
    "release",
    "bundle",
    "appimage_deb",
  );
  rmSync(stalePackageDirectory, { recursive: true, force: true });
  info(`cleared stale Tauri AppImage package staging: ${stalePackageDirectory}`);
  const cacheDirectory = tauriCacheDirectory();
  mkdirSync(cacheDirectory, { recursive: true });
  for (const tool of appImageToolsForArch(arch)) {
    await stageTool(tool, cacheDirectory);
  }
  ok(`verified AppImage toolchain staged for ${arch} in ${cacheDirectory}`);
}

main().catch((error: unknown) => {
  fail(error instanceof Error ? error.message : String(error));
});
