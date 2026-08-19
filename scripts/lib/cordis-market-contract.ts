// Pure contract helpers for the public Cordis market API release probe.
//
// These helpers observe the production wire contract only. They do not make
// an installation decision; src-tauri/src/market.rs remains the authoritative
// Desktop gate for source, integrity, engine, and activation safety.

export const CORDIS_MARKET_API_ORIGIN = "https://cordis.run/api/v1";
export const DEFAULT_MISSING_MARKET_SLUG = "cordis-production-probe-missing";

const MARKET_SLUG_RE = /^[a-z0-9][a-z0-9-]{0,127}$/;
const PACKAGE_NAME_RE = /^(@[a-z0-9-~][a-z0-9-._~]*\/)?[a-z0-9-~][a-z0-9-._~]*$/;
const EXACT_SEMVER_RE = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;
const SHA512_INTEGRITY_RE = /^sha512-[A-Za-z0-9+/]{86}==$/;
const DSH_ENGINE_RE = /^>=\s*(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\s+(?:<|<=)\s*(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)?$/;

type JsonRecord = Record<string, unknown>;

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function directResponseProblem(args: {
  endpoint: URL;
  status: number;
  statusText: string;
  contentType: string | null;
  location: string | null;
  expectedStatus: number;
}): string | null {
  const { endpoint, status, statusText, contentType, location, expectedStatus } = args;
  if (status !== expectedStatus) {
    const redirectHint = location === null ? "" : ` (Location: ${JSON.stringify(location)})`;
    return `expected direct HTTP ${expectedStatus} JSON from ${endpoint}, got ${status}` +
      `${statusText ? ` ${statusText}` : ""}${redirectHint}`;
  }
  if (location !== null) {
    return `expected no redirect Location from ${endpoint}, got ${JSON.stringify(location)}`;
  }
  if (!isJsonContentType(contentType)) {
    return `expected Content-Type application/json from ${endpoint}, got ${JSON.stringify(contentType)}`;
  }
  return null;
}

export function isValidCordisMarketSlug(value: string): boolean {
  return MARKET_SLUG_RE.test(value);
}

export function marketListUrl(limit = 1): URL {
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
    throw new Error(`market limit must be an integer in 1..100: ${JSON.stringify(limit)}`);
  }
  const url = new URL(`${CORDIS_MARKET_API_ORIGIN}/plugins`);
  url.searchParams.set("platform", "desktop");
  url.searchParams.set("limit", String(limit));
  return url;
}

export function marketDetailUrl(slug: string): URL {
  if (!isValidCordisMarketSlug(slug)) {
    throw new Error(`invalid Cordis market slug: ${JSON.stringify(slug)}`);
  }
  return new URL(`${CORDIS_MARKET_API_ORIGIN}/plugins/${slug}`);
}

export function isJsonContentType(contentType: string | null): boolean {
  return contentType?.split(";", 1)[0]?.trim().toLowerCase() === "application/json";
}

export function directJsonResponseProblem(args: {
  endpoint: URL;
  status: number;
  statusText: string;
  contentType: string | null;
  location: string | null;
}): string | null {
  return directResponseProblem({ ...args, expectedStatus: 200 });
}

export function directJsonErrorResponseProblem(args: {
  endpoint: URL;
  status: number;
  statusText: string;
  contentType: string | null;
  location: string | null;
  expectedStatus: number;
}): string | null {
  return directResponseProblem(args);
}

export function notModifiedResponseProblem(args: {
  endpoint: URL;
  status: number;
  statusText: string;
  location: string | null;
}): string | null {
  const { endpoint, status, statusText, location } = args;
  if (status !== 304) {
    const redirectHint = location === null ? "" : ` (Location: ${JSON.stringify(location)})`;
    return `expected HTTP 304 after If-None-Match from ${endpoint}, got ${status}` +
      `${statusText ? ` ${statusText}` : ""}${redirectHint}`;
  }
  if (location !== null) {
    return `expected no redirect Location from 304 ${endpoint}, got ${JSON.stringify(location)}`;
  }
  return null;
}

export function marketListResponseProblem(value: unknown): string | null {
  if (!isRecord(value)) return "market list body must be an object";
  if (value.schemaVersion !== 1) return "market list schemaVersion must be 1";
  if (!nonEmptyString(value.catalogRevision)) return "market list catalogRevision is required";
  if (!nonEmptyString(value.updated)) return "market list updated is required";
  if (!Number.isSafeInteger(value.count) || (value.count as number) < 0) {
    return "market list count must be a non-negative safe integer";
  }
  if (!Array.isArray(value.items)) return "market list items must be an array";
  if (!isRecord(value.categories)) return "market list categories must be an object";
  if (!isRecord(value.page)) return "market list page must be an object";
  if (value.page.cursor !== null && typeof value.page.cursor !== "string") {
    return "market list page.cursor must be string or null";
  }
  if (typeof value.page.hasMore !== "boolean") return "market list page.hasMore must be boolean";
  if (!Number.isSafeInteger(value.page.limit) || (value.page.limit as number) < 1 || (value.page.limit as number) > 100) {
    return "market list page.limit must be an integer in 1..100";
  }
  return null;
}

export function marketErrorResponseProblem(value: unknown, expectedCode: string): string | null {
  if (!isRecord(value) || !isRecord(value.error)) return "market error body must contain error object";
  if (value.error.code !== expectedCode) {
    return `market error code must be ${expectedCode}, got ${JSON.stringify(value.error.code)}`;
  }
  if (!nonEmptyString(value.error.message)) return "market error message is required";
  return null;
}

/**
 * Validate the strictly nested source shape that the Rust Desktop gate expects
 * before an explicitly requested release probe names a real plugin. It is
 * deliberately read-only and does not replace candidate_from_entry in Rust.
 */
export function desktopInstallWireProblem(value: unknown): string | null {
  if (!isRecord(value)) return "market detail body must be an object";
  if (!isValidCordisMarketSlug(String(value.slug ?? ""))) return "detail slug is invalid";
  if (!nonEmptyString(value.entryRevision)) return "detail entryRevision is required";
  if (!isRecord(value.description) || !nonEmptyString(value.description.zh) || !nonEmptyString(value.description.en)) {
    return "detail description.zh and description.en are required";
  }
  if (value.blocked !== false) return "detail must be explicitly unblocked";
  if (value.deprecated !== false) return "detail must be explicitly non-deprecated";
  if (!Array.isArray(value.platforms) || !value.platforms.includes("desktop")) {
    return "detail platforms must include desktop";
  }
  if (!isRecord(value.engines) || !nonEmptyString(value.engines.dsh) || !DSH_ENGINE_RE.test(value.engines.dsh)) {
    return "detail engines.dsh must be a standard >= DSH range";
  }
  if (!isRecord(value.source)) return "detail source is required";
  if (value.source.type !== "npm") return "detail source.type must be npm";
  if (!nonEmptyString(value.source.packageName) || !PACKAGE_NAME_RE.test(value.source.packageName)) {
    return "detail source.packageName is invalid";
  }
  if (!nonEmptyString(value.source.version) || !EXACT_SEMVER_RE.test(value.source.version)) {
    return "detail source.version must be an exact semver";
  }
  if (!nonEmptyString(value.source.integrity) || !SHA512_INTEGRITY_RE.test(value.source.integrity)) {
    return "detail source.integrity must be canonical sha512";
  }
  if (value.source.registry !== "https://registry.npmjs.org") {
    return "detail source.registry must be https://registry.npmjs.org";
  }
  if (!nonEmptyString(value.source.tarball)) return "detail source.tarball is required";
  try {
    const tarball = new URL(value.source.tarball);
    if (tarball.protocol !== "https:" || tarball.hostname !== "registry.npmjs.org" || tarball.port || tarball.username || tarball.password || tarball.search || tarball.hash || !tarball.pathname.endsWith(".tgz")) {
      return "detail source.tarball must be a direct https registry.npmjs.org .tgz URL";
    }
  } catch {
    return "detail source.tarball is invalid";
  }
  return null;
}
