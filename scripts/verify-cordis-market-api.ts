// Production release-contract probe for Cordis market API.
//
// This performs no mutation and never downloads or installs a plugin. It
// verifies the exact direct JSON, ETag/304, and JSON-404 behavior that the
// Desktop Rust client consumes. Set CORDIS_MARKET_PROBE_SLUG only after a
// reviewed public catalog entry exists to inspect its install wire shape.

import { randomUUID } from "node:crypto";
import {
  MISSING_MARKET_SLUG_PREFIX,
  desktopInstallWireProblem,
  directJsonErrorResponseProblem,
  directJsonResponseProblem,
  isValidCordisMarketSlug,
  marketDetailUrl,
  marketErrorResponseProblem,
  marketListResponseProblem,
  marketListUrl,
  notModifiedResponseProblem,
} from "./lib/cordis-market-contract.ts";
import { fail, info, ok } from "./lib/common.ts";

const REQUEST_TIMEOUT_MS = 30_000;
const JSON_HEADERS = { accept: "application/json" };

function requestedProbeSlug(): string | null {
  const slug = process.env.CORDIS_MARKET_PROBE_SLUG;
  if (slug === undefined || slug === "") return null;
  if (!isValidCordisMarketSlug(slug)) {
    throw new Error(`CORDIS_MARKET_PROBE_SLUG is invalid: ${JSON.stringify(slug)}`);
  }
  return slug;
}

async function fetchJson(endpoint: URL): Promise<{ body: unknown; etag: string }> {
  const response = await fetch(endpoint, {
    redirect: "manual",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    headers: JSON_HEADERS,
  });
  const problem = directJsonResponseProblem({
    endpoint,
    status: response.status,
    statusText: response.statusText,
    contentType: response.headers.get("content-type"),
    location: response.headers.get("location"),
  });
  if (problem !== null) {
    await response.body?.cancel().catch(() => undefined);
    throw new Error(problem);
  }
  const etag = response.headers.get("etag");
  if (etag === null || etag.trim().length === 0) {
    await response.body?.cancel().catch(() => undefined);
    throw new Error(`expected ETag from ${endpoint}`);
  }
  let body: unknown;
  try {
    body = await response.json();
  } catch (error) {
    throw new Error(`could not parse JSON from ${endpoint}: ${error instanceof Error ? error.message : String(error)}`);
  }
  return { body, etag };
}

async function assertNotModified(endpoint: URL, etag: string): Promise<void> {
  const response = await fetch(endpoint, {
    redirect: "manual",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    headers: { ...JSON_HEADERS, "if-none-match": etag },
  });
  try {
    const problem = notModifiedResponseProblem({
      endpoint,
      status: response.status,
      statusText: response.statusText,
      location: response.headers.get("location"),
    });
    if (problem !== null) throw new Error(problem);
  } finally {
    await response.body?.cancel().catch(() => undefined);
  }
}

async function assertJson404(): Promise<void> {
  // A fixed sentinel can eventually collide with a real catalog entry and
  // turn a healthy production API into a false release failure. Keep the
  // request inside the validated slug grammar while making collision
  // practically impossible for every probe run.
  const missingSlug = `${MISSING_MARKET_SLUG_PREFIX}-${randomUUID()}`;
  const endpoint = marketDetailUrl(missingSlug);
  const response = await fetch(endpoint, {
    redirect: "manual",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    headers: JSON_HEADERS,
  });
  const problem = directJsonErrorResponseProblem({
    endpoint,
    status: response.status,
    statusText: response.statusText,
    contentType: response.headers.get("content-type"),
    location: response.headers.get("location"),
    expectedStatus: 404,
  });
  if (problem !== null) {
    await response.body?.cancel().catch(() => undefined);
    throw new Error(problem);
  }
  let body: unknown;
  try {
    body = await response.json();
  } catch (error) {
    throw new Error(`could not parse JSON 404 from ${endpoint}: ${error instanceof Error ? error.message : String(error)}`);
  }
  const bodyProblem = marketErrorResponseProblem(body, "NOT_FOUND");
  if (bodyProblem !== null) throw new Error(`${endpoint}: ${bodyProblem}`);
}

async function main(): Promise<void> {
  const listEndpoint = marketListUrl(1);
  info(`probing Cordis Desktop market response: ${listEndpoint}`);
  const list = await fetchJson(listEndpoint);
  const listProblem = marketListResponseProblem(list.body);
  if (listProblem !== null) throw new Error(`${listEndpoint}: ${listProblem}`);
  await assertNotModified(listEndpoint, list.etag);
  await assertJson404();

  const probeSlug = requestedProbeSlug();
  if (probeSlug === null) {
    const count = (list.body as { count: number }).count;
    info(`catalog structural probe passed (desktop count=${count}); no CORDIS_MARKET_PROBE_SLUG supplied, so no plugin was inspected or installed`);
  } else {
    const endpoint = marketDetailUrl(probeSlug);
    info(`probing requested Desktop install wire shape: ${endpoint}`);
    const detail = await fetchJson(endpoint);
    const detailProblem = desktopInstallWireProblem(detail.body);
    if (detailProblem !== null) throw new Error(`${endpoint}: ${detailProblem}`);
    await assertNotModified(endpoint, detail.etag);
    ok(`Cordis market ${probeSlug} matches the Desktop nested-source compatibility probe; no install was performed`);
  }

  ok("Cordis Desktop market API returns direct JSON, ETag/304, and JSON 404 as contracted");
}

main().catch((error: unknown) => {
  fail(error instanceof Error ? error.message : String(error));
});
