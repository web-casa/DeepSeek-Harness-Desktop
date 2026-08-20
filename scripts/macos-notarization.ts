// Persisted macOS notarization state machine.
//
// `submit` uploads exactly once and writes target/notarization/<target>.json.
// The successful build job uploads that state with the signed DMG. `wait`
// runs in a separate job, polls the recorded ID for at most 20 minutes, then
// staples and verifies the accepted DMG. Re-running only failed jobs therefore
// resumes the original Apple submission instead of uploading a duplicate.

import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  createReadStream,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  renameSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join } from "node:path";
import { ascCliPath } from "./lib/asc-cli.ts";
import { fail, info, ok, repoRoot } from "./lib/common.ts";
import {
  isRetryableNotarizationError,
  isTerminalNotarizationAuthError,
  extractSubmissionId,
  notarizationPollBackoff,
  notarizationStateProblems,
  parseNotarizationResponse,
  type NotarizationProvider,
  type NotarizationState,
  type NotarizationStatus,
} from "./lib/macos-notarization.ts";
import {
  bundleArtifactCandidates,
  targetById,
  type NativeReleaseTarget,
} from "./lib/release-artifacts.ts";

interface CommandResult {
  status: number | null;
  stdout: string;
  stderr: string;
  error?: Error;
}

const STATUS_QUERY_TIMEOUT_MS = 90_000;
const SUBMIT_TIMEOUT_MS = 10 * 60_000;
const LOG_TIMEOUT_MS = 2 * 60_000;
const STAPLE_TIMEOUT_MS = 5 * 60_000;
const DEFAULT_WAIT_MINUTES = 20;
const DEFAULT_POLL_INTERVAL_MS = 60_000;
const SUBMIT_ATTEMPTS = 2;
const SUBMIT_RETRY_DELAY_MS = 30_000;

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function requestedTarget(): NativeReleaseTarget {
  const targetId = argument("--target");
  if (!targetId) fail("usage: node scripts/macos-notarization.ts <submit|wait> --target macos-<arch>");
  const target = targetById(targetId);
  if (!target || !target.id.startsWith("macos-")) fail(`unknown macOS release target: ${targetId}`);
  if (process.platform !== "darwin") fail("macOS notarization must run on macOS");
  if (process.arch !== target.arch) {
    fail(`target ${target.id} requires ${target.arch}, current process is ${process.arch}`);
  }
  return target;
}

function notarizationDirectory(): string {
  return join(repoRoot, "target", "notarization");
}

function statePath(target: NativeReleaseTarget): string {
  return join(notarizationDirectory(), `${target.id}.json`);
}

function developerLogPath(target: NativeReleaseTarget): string {
  return join(notarizationDirectory(), `${target.id}-developer-log.json`);
}

function findDmg(): string {
  const candidates = bundleArtifactCandidates(repoRoot, "dmg");
  if (candidates.length !== 1) {
    fail(`expected exactly one DMG artifact, found ${candidates.join(", ") || "none"}`);
  }
  const metadata = lstatSync(candidates[0]);
  if (metadata.isSymbolicLink() || !metadata.isFile() || metadata.size <= 0) {
    fail("refusing to notarize a symlink, non-regular, or empty DMG");
  }
  return candidates[0];
}

async function sha256File(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

function providerFromEnvironment(): NotarizationProvider {
  const value = process.env.APPLE_NOTARIZATION_PROVIDER;
  if (value !== "asc" && value !== "notarytool") {
    fail("APPLE_NOTARIZATION_PROVIDER must be asc or notarytool");
  }
  return value;
}

function requiredEnvironment(name: string): string {
  const value = process.env[name];
  if (value === undefined || value.trim() === "") fail(`${name} is required for notarization`);
  return value;
}

function providerSecrets(provider: NotarizationProvider): string[] {
  return provider === "asc"
    ? ["ASC_KEY_ID", "ASC_ISSUER_ID", "ASC_PRIVATE_KEY_B64"]
        .map((name) => process.env[name])
        .filter((value): value is string => Boolean(value))
    : ["APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID"]
        .map((name) => process.env[name])
        .filter((value): value is string => Boolean(value));
}

function redact(text: string, provider: NotarizationProvider): string {
  let redacted = text;
  for (const secret of providerSecrets(provider)) {
    if (secret.length >= 4) redacted = redacted.replaceAll(secret, "***");
  }
  return redacted;
}

function command(
  executable: string,
  args: string[],
  provider: NotarizationProvider,
  timeout: number,
): CommandResult {
  const result = spawnSync(executable, args, {
    encoding: "utf8",
    env: provider === "asc" ? { ...process.env, ASC_STRICT_AUTH: "true" } : process.env,
    timeout,
    killSignal: "SIGTERM",
    maxBuffer: 10 * 1024 * 1024,
  });
  return {
    status: result.status,
    stdout: redact(result.stdout ?? "", provider),
    stderr: redact(result.stderr ?? "", provider),
    error: result.error,
  };
}

function notarytoolAuthArgs(): string[] {
  return [
    "--apple-id",
    requiredEnvironment("APPLE_ID"),
    "--password",
    requiredEnvironment("APPLE_PASSWORD"),
    "--team-id",
    requiredEnvironment("APPLE_TEAM_ID"),
  ];
}

function assertAscReady(): string {
  requiredEnvironment("ASC_KEY_ID");
  requiredEnvironment("ASC_ISSUER_ID");
  requiredEnvironment("ASC_PRIVATE_KEY_B64");
  const executable = ascCliPath();
  if (!existsSync(executable) || !lstatSync(executable).isFile()) {
    fail(`verified ASC CLI is missing at ${executable}; run scripts/prepare-asc-cli.ts`);
  }
  return executable;
}

function submitDmg(provider: NotarizationProvider, dmg: string): CommandResult {
  if (provider === "asc") {
    return command(
      assertAscReady(),
      ["notarization", "submit", "--file", dmg, "--output", "json"],
      provider,
      SUBMIT_TIMEOUT_MS,
    );
  }
  return command(
    "xcrun",
    [
      "notarytool",
      "submit",
      dmg,
      ...notarytoolAuthArgs(),
      "--output-format",
      "json",
    ],
    provider,
    SUBMIT_TIMEOUT_MS,
  );
}

function queryStatus(provider: NotarizationProvider, submissionId: string): CommandResult {
  if (provider === "asc") {
    return command(
      assertAscReady(),
      ["notarization", "status", "--id", submissionId, "--output", "json"],
      provider,
      STATUS_QUERY_TIMEOUT_MS,
    );
  }
  return command(
    "xcrun",
    [
      "notarytool",
      "info",
      submissionId,
      ...notarytoolAuthArgs(),
      "--output-format",
      "json",
    ],
    provider,
    STATUS_QUERY_TIMEOUT_MS,
  );
}

function fetchDeveloperLog(
  provider: NotarizationProvider,
  submissionId: string,
): CommandResult {
  if (provider === "asc") {
    return command(
      assertAscReady(),
      ["notarization", "log", "--id", submissionId, "--output", "json"],
      provider,
      LOG_TIMEOUT_MS,
    );
  }
  return command(
    "xcrun",
    ["notarytool", "log", submissionId, ...notarytoolAuthArgs()],
    provider,
    LOG_TIMEOUT_MS,
  );
}

function commandFailure(result: CommandResult): string {
  const output = `${result.stderr}\n${result.stdout}`.trim();
  return output || result.error?.message || `exit ${String(result.status)}`;
}

function writeState(target: NativeReleaseTarget, state: NotarizationState): void {
  const problems = notarizationStateProblems(state, target.id);
  if (problems.length > 0) fail(`refusing to persist invalid notarization state:\n- ${problems.join("\n- ")}`);
  const path = statePath(target);
  mkdirSync(dirname(path), { recursive: true });
  const partial = `${path}.tmp-${process.pid}`;
  writeFileSync(partial, `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
  renameSync(partial, path);
}

function readState(target: NativeReleaseTarget): NotarizationState {
  const path = statePath(target);
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`cannot read notarization state ${path}: ${error instanceof Error ? error.message : String(error)}`);
  }
  const problems = notarizationStateProblems(parsed, target.id);
  if (problems.length > 0) fail(`invalid notarization state artifact:\n- ${problems.join("\n- ")}`);
  return parsed as NotarizationState;
}

function waitMinutes(): number {
  const raw = process.env.DSH_NOTARIZATION_WAIT_MINUTES ?? String(DEFAULT_WAIT_MINUTES);
  const value = Number(raw);
  if (!Number.isInteger(value) || value < 1 || value > 120) {
    fail("DSH_NOTARIZATION_WAIT_MINUTES must be an integer from 1 to 120");
  }
  return value;
}

function pollIntervalMs(): number {
  const raw = process.env.DSH_NOTARIZATION_POLL_SECONDS ?? String(DEFAULT_POLL_INTERVAL_MS / 1000);
  const seconds = Number(raw);
  if (!Number.isInteger(seconds) || seconds < 15 || seconds > 300) {
    fail("DSH_NOTARIZATION_POLL_SECONDS must be an integer from 15 to 300");
  }
  return seconds * 1000;
}

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function submit(target: NativeReleaseTarget): Promise<void> {
  const provider = providerFromEnvironment();
  const dmg = findDmg();
  const artifactSha256 = await sha256File(dmg);
  if (existsSync(statePath(target))) {
    const existing = readState(target);
    if (existing.artifactSha256 !== artifactSha256 || existing.artifactName !== basename(dmg)) {
      fail("existing notarization state does not belong to the current DMG");
    }
    ok(`reusing recorded notarization submission ${existing.submissionId}; no upload performed`);
    return;
  }

  info(`submitting signed ${target.id} DMG through ${provider} without waiting`);
  for (let attempt = 1; attempt <= SUBMIT_ATTEMPTS; attempt += 1) {
    const result = submitDmg(provider, dmg);
    if (result.status === 0) {
      const response = parseNotarizationResponse(provider, result.stdout);
      const state: NotarizationState = {
        schemaVersion: 1,
        target: target.id as `macos-${typeof target.arch}`,
        arch: target.arch,
        provider,
        submissionId: response.submissionId,
        artifactName: basename(dmg),
        artifactSha256,
        status: response.status,
        createdAt: new Date().toISOString(),
      };
      writeState(target, state);
      ok(`Apple accepted upload ${response.submissionId} (${response.status}); wait is deferred`);
      return;
    }

    const failure = commandFailure(result);
    const observedSubmissionId = extractSubmissionId(failure);
    if (observedSubmissionId !== null) {
      const state: NotarizationState = {
        schemaVersion: 1,
        target: target.id as `macos-${typeof target.arch}`,
        arch: target.arch,
        provider,
        submissionId: observedSubmissionId,
        artifactName: basename(dmg),
        artifactSha256,
        status: "In Progress",
        createdAt: new Date().toISOString(),
      };
      writeState(target, state);
      console.warn(
        `::warning::upload command failed after Apple issued ${observedSubmissionId}; preserving that ID and refusing a duplicate submit`,
      );
      return;
    }
    if (
      attempt < SUBMIT_ATTEMPTS &&
      isRetryableNotarizationError(failure) &&
      !isTerminalNotarizationAuthError(failure)
    ) {
      info(
        `transient upload failure before any Submission ID was observed; retrying once in ${SUBMIT_RETRY_DELAY_MS / 1000}s`,
      );
      await sleep(SUBMIT_RETRY_DELAY_MS);
      continue;
    }
    fail(`notarization upload failed before a Submission ID was recorded: ${failure}`);
  }
}

function persistDeveloperLog(
  target: NativeReleaseTarget,
  provider: NotarizationProvider,
  submissionId: string,
): void {
  const result = fetchDeveloperLog(provider, submissionId);
  const path = developerLogPath(target);
  mkdirSync(dirname(path), { recursive: true });
  const content = result.stdout.trim() || result.stderr.trim() || result.error?.message || "log unavailable";
  writeFileSync(path, `${content}\n`, { mode: 0o600 });
  if (result.status === 0) info(`developer log saved to ${path}`);
  else info(`developer log request failed; diagnostic saved to ${path}`);
}

function stapleDmg(provider: NotarizationProvider, dmg: string): void {
  for (const [verb, timeout] of [
    ["staple", STAPLE_TIMEOUT_MS],
    ["validate", STAPLE_TIMEOUT_MS],
  ] as const) {
    const result = command("xcrun", ["stapler", verb, dmg], provider, timeout);
    if (result.status !== 0) fail(`stapler ${verb} failed: ${commandFailure(result)}`);
  }
}

async function waitForResult(target: NativeReleaseTarget): Promise<void> {
  const state = readState(target);
  const dmg = findDmg();
  const artifactSha256 = await sha256File(dmg);
  if (basename(dmg) !== state.artifactName || artifactSha256 !== state.artifactSha256) {
    fail("notarization state hash/name does not match the downloaded DMG");
  }

  const deadline = Date.now() + waitMinutes() * 60_000;
  const interval = pollIntervalMs();
  let status: NotarizationStatus = state.status;
  let consecutiveFailures = 0;

  while (status === "In Progress") {
    if (Date.now() >= deadline) {
      writeState(target, { ...state, status, lastCheckedAt: new Date().toISOString() });
      fail(
        `notarization ${state.submissionId} is still In Progress after ${waitMinutes()} minutes; ` +
          "use GitHub Actions 'Re-run failed jobs' to resume this ID without uploading again",
      );
    }

    const result = queryStatus(state.provider, state.submissionId);
    if (result.status === 0) {
      const response = parseNotarizationResponse(
        state.provider,
        result.stdout,
        state.submissionId,
      );
      status = response.status;
      consecutiveFailures = 0;
      writeState(target, { ...state, status, lastCheckedAt: new Date().toISOString() });
      info(`notarization ${state.submissionId}: ${status}`);
      if (status !== "In Progress") break;
      await sleep(Math.min(interval, Math.max(0, deadline - Date.now())));
      continue;
    }

    const failure = commandFailure(result);
    if (isTerminalNotarizationAuthError(failure)) {
      fail(`notarization status authentication failed: ${failure}`);
    }
    if (!isRetryableNotarizationError(failure)) {
      fail(`notarization status check failed with a non-retryable error: ${failure}`);
    }
    consecutiveFailures += 1;
    const delay = notarizationPollBackoff(interval, consecutiveFailures);
    info(`transient notarization status failure; retrying the same ID in ${delay / 1000}s`);
    await sleep(Math.min(delay, Math.max(0, deadline - Date.now())));
  }

  if (status === "Invalid" || status === "Rejected") {
    persistDeveloperLog(target, state.provider, state.submissionId);
    fail(`Apple notarization ${state.submissionId} finished with status ${status}`);
  }
  if (status !== "Accepted") fail(`unexpected terminal notarization status: ${status}`);

  stapleDmg(state.provider, dmg);
  const completedAt = new Date().toISOString();
  writeState(target, {
    ...state,
    status: "Accepted",
    lastCheckedAt: completedAt,
    completedAt,
  });
  ok(`notarization ${state.submissionId} accepted; ticket stapled and validated`);
}

const operation = process.argv[2];
const target = requestedTarget();
if (operation === "submit") await submit(target);
else if (operation === "wait") await waitForResult(target);
else fail("usage: node scripts/macos-notarization.ts <submit|wait> --target macos-<arch>");
