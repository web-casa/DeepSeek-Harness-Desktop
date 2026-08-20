import type { ReleaseArch } from "./release-artifacts.ts";

export type NotarizationProvider = "asc" | "notarytool";
export type NotarizationStatus = "In Progress" | "Accepted" | "Invalid" | "Rejected";

export interface MacosSigningConfiguration {
  configured: boolean;
  provider: NotarizationProvider | null;
  problems: string[];
}

export interface NotarizationState {
  schemaVersion: 1;
  target: `macos-${ReleaseArch}`;
  arch: ReleaseArch;
  provider: NotarizationProvider;
  submissionId: string;
  artifactName: string;
  artifactSha256: string;
  status: NotarizationStatus;
  createdAt: string;
  lastCheckedAt?: string;
  completedAt?: string;
}

type Env = Readonly<Record<string, string | undefined>>;

function present(value: string | undefined): boolean {
  return value !== undefined && value.trim() !== "";
}

function completeness(env: Env, names: readonly string[]): "absent" | "partial" | "complete" {
  const count = names.filter((name) => present(env[name])).length;
  if (count === 0) return "absent";
  return count === names.length ? "complete" : "partial";
}

export function resolveMacosSigningConfiguration(env: Env): MacosSigningConfiguration {
  const problems: string[] = [];
  const certificate = present(env.APPLE_CERTIFICATE);
  const dependentNames = [
    "APPLE_CERTIFICATE_PASSWORD",
    "APPLE_TEAM_ID",
    "APPLE_ID",
    "APPLE_PASSWORD",
    "ASC_KEY_ID",
    "ASC_ISSUER_ID",
    "ASC_PRIVATE_KEY_B64",
  ] as const;

  if (!certificate) {
    const configuredDependents = dependentNames.filter((name) => present(env[name]));
    if (configuredDependents.length > 0) {
      problems.push(
        `partial Apple signing configuration: APPLE_CERTIFICATE is missing while ${configuredDependents.join(", ")} is configured`,
      );
    }
    return { configured: false, provider: null, problems };
  }

  for (const required of ["APPLE_CERTIFICATE_PASSWORD", "APPLE_TEAM_ID"] as const) {
    if (!present(env[required])) problems.push(`${required} is required with APPLE_CERTIFICATE`);
  }
  if (present(env.APPLE_TEAM_ID) && !/^[A-Z0-9]{10}$/.test(env.APPLE_TEAM_ID!.trim())) {
    problems.push("APPLE_TEAM_ID must be a 10-character uppercase alphanumeric Team ID");
  }

  const appleIdAuth = completeness(env, ["APPLE_ID", "APPLE_PASSWORD"]);
  const ascAuth = completeness(env, ["ASC_KEY_ID", "ASC_ISSUER_ID", "ASC_PRIVATE_KEY_B64"]);
  if (appleIdAuth === "partial") {
    problems.push("APPLE_ID and APPLE_PASSWORD must be configured together");
  }
  if (ascAuth === "partial") {
    problems.push("ASC_KEY_ID, ASC_ISSUER_ID and ASC_PRIVATE_KEY_B64 must be configured together");
  }

  const provider: NotarizationProvider | null =
    ascAuth === "complete" ? "asc" : appleIdAuth === "complete" ? "notarytool" : null;
  if (provider === null && appleIdAuth !== "partial" && ascAuth !== "partial") {
    problems.push(
      "notarization credentials are missing: configure either ASC API-key secrets or APPLE_ID + APPLE_PASSWORD",
    );
  }
  return { configured: true, provider, problems };
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

export function normalizeNotarizationStatus(value: unknown): NotarizationStatus | null {
  if (typeof value !== "string") return null;
  const normalized = value.trim().toLowerCase().replace(/[\s_-]+/g, " ");
  switch (normalized) {
    case "in progress":
    case "inprogress":
      return "In Progress";
    case "accepted":
      return "Accepted";
    case "invalid":
      return "Invalid";
    case "rejected":
      return "Rejected";
    default:
      return null;
  }
}

export interface ParsedNotarizationResponse {
  submissionId: string;
  status: NotarizationStatus;
}

interface RawNotarizationResponse {
  submissionId: string;
  statusValue: unknown;
}

function parseRawNotarizationResponse(
  provider: NotarizationProvider,
  json: string,
  expectedSubmissionId?: string,
): RawNotarizationResponse {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    throw new Error(`${provider} returned non-JSON notarization output`);
  }
  const root = record(parsed);
  if (root === null) throw new Error(`${provider} returned a non-object notarization response`);

  let submissionId: unknown;
  let statusValue: unknown;
  if (provider === "asc") {
    const data = record(root.data);
    const attributes = record(data?.attributes);
    submissionId = data?.id;
    statusValue = attributes?.status;
  } else {
    submissionId = root.id;
    statusValue = root.status;
  }

  if (typeof submissionId !== "string" || !isSubmissionId(submissionId)) {
    throw new Error(`${provider} response is missing a valid notarization submission ID`);
  }
  if (expectedSubmissionId !== undefined && submissionId !== expectedSubmissionId) {
    throw new Error(
      `${provider} returned submission ${submissionId}, expected ${expectedSubmissionId}`,
    );
  }
  return { submissionId, statusValue };
}

export function parseNotarizationResponse(
  provider: NotarizationProvider,
  json: string,
  expectedSubmissionId?: string,
): ParsedNotarizationResponse {
  const { submissionId, statusValue } = parseRawNotarizationResponse(
    provider,
    json,
    expectedSubmissionId,
  );
  const status = normalizeNotarizationStatus(statusValue);
  if (status === null) throw new Error(`${provider} returned an unknown notarization status`);
  return { submissionId, status };
}

// A successful fire-and-forget upload is not a status response. In particular,
// notarytool may return only `id`, `message`, and `path` until `--wait` is used.
// Once a valid ID is present and the command exits successfully, persist it as
// In Progress and let the separate status query determine the real state.
export function parseNotarizationSubmissionResponse(
  provider: NotarizationProvider,
  json: string,
): ParsedNotarizationResponse {
  const { submissionId, statusValue } = parseRawNotarizationResponse(provider, json);
  return {
    submissionId,
    status: normalizeNotarizationStatus(statusValue) ?? "In Progress",
  };
}

export function isSubmissionId(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
    value,
  );
}

export function extractSubmissionId(output: string): string | null {
  const patterns = [
    /\bSubmission created:\s*([0-9a-f-]{36})\b/i,
    /\/notary\/v\d+\/submissions\/([0-9a-f-]{36})\b/i,
    /\bsubmission(?:Id| ID)?["'\s:=]+([0-9a-f-]{36})\b/i,
  ];
  for (const pattern of patterns) {
    const match = output.match(pattern);
    if (match && isSubmissionId(match[1])) return match[1];
  }
  return null;
}

export function notarizationStateProblems(
  value: unknown,
  expectedTarget?: string,
): string[] {
  const problems: string[] = [];
  const state = record(value);
  if (state === null) return ["state must be a JSON object"];
  if (state.schemaVersion !== 1) problems.push("schemaVersion must be 1");
  if (state.arch !== "x64" && state.arch !== "arm64") problems.push("arch must be x64 or arm64");
  if (state.target !== `macos-${String(state.arch)}`) {
    problems.push("target must match macos-<arch>");
  }
  if (expectedTarget !== undefined && state.target !== expectedTarget) {
    problems.push(`state target ${String(state.target)} does not match ${expectedTarget}`);
  }
  if (state.provider !== "asc" && state.provider !== "notarytool") {
    problems.push("provider must be asc or notarytool");
  }
  if (typeof state.submissionId !== "string" || !isSubmissionId(state.submissionId)) {
    problems.push("submissionId must be a UUID");
  }
  if (
    typeof state.artifactName !== "string" ||
    state.artifactName === "" ||
    state.artifactName.includes("/") ||
    state.artifactName.includes("\\") ||
    !state.artifactName.endsWith(".dmg")
  ) {
    problems.push("artifactName must be a basename ending in .dmg");
  }
  if (typeof state.artifactSha256 !== "string" || !/^[a-f0-9]{64}$/.test(state.artifactSha256)) {
    problems.push("artifactSha256 must be a lowercase SHA-256");
  }
  if (normalizeNotarizationStatus(state.status) === null) {
    problems.push("status is not a recognized notarization status");
  }
  for (const field of ["createdAt", "lastCheckedAt", "completedAt"] as const) {
    const timestamp = state[field];
    if (timestamp !== undefined && (typeof timestamp !== "string" || !isIsoTimestamp(timestamp))) {
      problems.push(`${field} must be an ISO timestamp`);
    }
  }
  return problems;
}

function isIsoTimestamp(value: string): boolean {
  const time = Date.parse(value);
  return Number.isFinite(time) && new Date(time).toISOString() === value;
}

export function isRetryableNotarizationError(output: string): boolean {
  return (
    /NSURLErrorDomain Code=-1009\b/i.test(output) ||
    /\bNo network route\b/i.test(output) ||
    /Internet connection appears to be offline/i.test(output) ||
    /\b(?:HTTP|status(?:Code)?)\D*(?:408|429|500|502|503|504)\b/i.test(output) ||
    /\b(?:ETIMEDOUT|ECONNRESET|ECONNREFUSED|ENETUNREACH|EAI_AGAIN)\b/i.test(output) ||
    /\b(?:timed out|timeout|temporary failure|unexpected EOF)\b/i.test(output)
  );
}

export function isTerminalNotarizationAuthError(output: string): boolean {
  return (
    /\b(?:HTTP|status(?:Code)?)\D*(?:401|403)\b/i.test(output) ||
    /\b(?:unauthorized|forbidden|invalid credentials|authentication failed)\b/i.test(output)
  );
}

export function notarizationPollBackoff(
  pollIntervalMs: number,
  consecutiveFailures: number,
): number {
  const exponent = Math.max(0, consecutiveFailures - 1);
  return Math.min(Math.max(pollIntervalMs, pollIntervalMs * 2 ** exponent), 120_000);
}
