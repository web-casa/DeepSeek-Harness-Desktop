// Retry one complete signed macOS build only when Apple notarization loses its
// network route. Deterministic signing, authentication, or validation failures
// remain fail-fast so CI never hides a real release defect.

import { spawn } from "node:child_process";
import { fail, info, ok } from "./lib/common.ts";
import { isRetryableAppleNotarizationNetworkError } from "./lib/macos-signing.ts";

if (process.platform !== "darwin") fail("macOS release bundling must run on macOS");

const maxAttempts = 2;
const retryDelayMs = 30_000;
const diagnosticTailLimit = 2 * 1024 * 1024;

interface AttemptResult {
  code: number | null;
  signal: NodeJS.Signals | null;
  diagnosticTail: string;
  spawnError?: string;
}

function appendTail(current: string, chunk: Buffer): string {
  const combined = current + chunk.toString("utf8");
  return combined.length > diagnosticTailLimit
    ? combined.slice(-diagnosticTailLimit)
    : combined;
}

function runBuild(): Promise<AttemptResult> {
  return new Promise((resolve) => {
    const child = spawn("pnpm", ["tauri", "build", "--bundles", "dmg,app"], {
      env: process.env,
      stdio: ["inherit", "pipe", "pipe"],
    });
    let diagnosticTail = "";
    let spawnError: string | undefined;

    child.stdout.on("data", (chunk: Buffer) => {
      process.stdout.write(chunk);
      diagnosticTail = appendTail(diagnosticTail, chunk);
    });
    child.stderr.on("data", (chunk: Buffer) => {
      process.stderr.write(chunk);
      diagnosticTail = appendTail(diagnosticTail, chunk);
    });
    child.once("error", (error) => {
      spawnError = error.message;
    });
    child.once("close", (code, signal) => {
      resolve({ code, signal, diagnosticTail, spawnError });
    });
  });
}

for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
  info(`signed macOS bundle attempt ${attempt}/${maxAttempts}`);
  const result = await runBuild();
  if (result.code === 0 && !result.spawnError) {
    ok(`signed and notarized macOS bundle completed on attempt ${attempt}`);
    process.exitCode = 0;
    break;
  }

  const retryable = isRetryableAppleNotarizationNetworkError(result.diagnosticTail);
  if (attempt < maxAttempts && retryable) {
    console.warn(
      `::warning::Apple notarization lost its network route; retrying the complete signed macOS bundle in ${retryDelayMs / 1000}s`,
    );
    await new Promise((resolve) => setTimeout(resolve, retryDelayMs));
    continue;
  }

  const reason = result.spawnError
    ? `spawn error: ${result.spawnError}`
    : result.signal
      ? `signal ${result.signal}`
      : `exit ${result.code ?? "unknown"}`;
  fail(
    retryable
      ? `signed macOS bundle failed after ${attempt} network-retry attempts (${reason})`
      : `signed macOS bundle failed with a non-retryable error (${reason})`,
  );
}
