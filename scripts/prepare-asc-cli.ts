// Download the optional ASC notarization client from an immutable release and
// verify it before execution. This path is used only when ASC API-key secrets
// are configured; Apple-ID builds use the Xcode-provided notarytool.

import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  createReadStream,
  existsSync,
  mkdirSync,
  renameSync,
  rmSync,
} from "node:fs";
import { dirname } from "node:path";
import { open } from "node:fs/promises";
import {
  ascCliDefinitionProblems,
  ascCliDistribution,
  ascCliDownloadUrl,
  ascCliPath,
  ascCliRelease,
} from "./lib/asc-cli.ts";
import { fail, info, ok } from "./lib/common.ts";
import type { ReleaseArch } from "./lib/release-artifacts.ts";

const DOWNLOAD_TIMEOUT_MS = 3 * 60_000;
const DOWNLOAD_RETRIES = 3;
const MAX_BINARY_BYTES = 64 * 1024 * 1024;

function requestedArch(): ReleaseArch {
  const index = process.argv.indexOf("--arch");
  const value = index >= 0 ? process.argv[index + 1] : process.arch;
  if (value !== "x64" && value !== "arm64") fail(`unsupported ASC CLI architecture: ${value}`);
  return value;
}

async function sha256File(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

function probeBinary(path: string): void {
  const result = spawnSync(path, ["--version"], { encoding: "utf8", timeout: 30_000 });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  if (result.status !== 0 || !output.includes(ascCliRelease.version)) {
    fail(`verified ASC CLI failed its version probe: ${result.error?.message ?? output.trim()}`);
  }
}

async function downloadOnce(url: URL, destination: string, expectedSha256: string): Promise<void> {
  const response = await fetch(url, {
    redirect: "follow",
    headers: { "user-agent": "DeepSeek-Harness-Desktop-release" },
    signal: AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS),
  });
  if (!response.ok || response.body === null) throw new Error(`HTTP ${response.status}`);
  const declaredSize = Number(response.headers.get("content-length") ?? "0");
  if (Number.isFinite(declaredSize) && declaredSize > MAX_BINARY_BYTES) {
    throw new Error(`declared size ${declaredSize} exceeds ${MAX_BINARY_BYTES} bytes`);
  }

  const file = await open(destination, "wx", 0o700);
  const hash = createHash("sha256");
  let received = 0;
  try {
    for await (const chunk of response.body) {
      received += chunk.byteLength;
      if (received > MAX_BINARY_BYTES) throw new Error(`download exceeds ${MAX_BINARY_BYTES} bytes`);
      hash.update(chunk);
      let offset = 0;
      while (offset < chunk.byteLength) {
        const { bytesWritten } = await file.write(chunk, offset, chunk.byteLength - offset);
        if (bytesWritten <= 0) throw new Error("download write made no progress");
        offset += bytesWritten;
      }
    }
  } finally {
    await file.close();
  }
  const actualSha256 = hash.digest("hex");
  if (actualSha256 !== expectedSha256) {
    throw new Error(`SHA-256 mismatch: expected ${expectedSha256}, got ${actualSha256}`);
  }
}

async function main(): Promise<void> {
  if (process.platform !== "darwin") fail("ASC CLI release staging must run on macOS");
  const problems = ascCliDefinitionProblems();
  if (problems.length > 0) fail(problems.join("\n"));

  const arch = requestedArch();
  const distribution = ascCliDistribution(arch);
  const destination = ascCliPath();
  mkdirSync(dirname(destination), { recursive: true });
  if (existsSync(destination) && (await sha256File(destination)) === distribution.sha256) {
    chmodSync(destination, 0o700);
    probeBinary(destination);
    ok(`verified cached ASC CLI ${arch} at ${destination}`);
    return;
  }

  const partial = `${destination}.download-${process.pid}-${Date.now()}`;
  rmSync(partial, { force: true });
  let lastError: unknown;
  for (let attempt = 1; attempt <= DOWNLOAD_RETRIES; attempt += 1) {
    try {
      info(`downloading ASC CLI ${arch} (attempt ${attempt})`);
      await downloadOnce(ascCliDownloadUrl(arch), partial, distribution.sha256);
      chmodSync(partial, 0o700);
      renameSync(partial, destination);
      probeBinary(destination);
      ok(`ASC CLI ${arch} ${distribution.file} verified at ${destination}`);
      return;
    } catch (error) {
      lastError = error;
      rmSync(partial, { force: true });
    }
  }
  fail(
    `ASC CLI download failed after ${DOWNLOAD_RETRIES} attempts: ${lastError instanceof Error ? lastError.message : String(lastError)}`,
  );
}

await main();
