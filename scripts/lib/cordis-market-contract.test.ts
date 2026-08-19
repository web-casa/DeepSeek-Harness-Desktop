import assert from "node:assert/strict";
import { test } from "node:test";
import {
  CORDIS_MARKET_API_ORIGIN,
  MISSING_MARKET_SLUG_PREFIX,
  desktopInstallWireProblem,
  directJsonErrorResponseProblem,
  directJsonResponseProblem,
  isJsonContentType,
  isValidCordisMarketSlug,
  marketDetailUrl,
  marketErrorResponseProblem,
  marketListResponseProblem,
  marketListUrl,
  notModifiedResponseProblem,
} from "./cordis-market-contract.ts";

const INTEGRITY = `sha512-${Buffer.alloc(64).toString("base64")}`;
const CANDIDATE_DSH_ENGINE = ">=0.1.0-rc.7 <0.2.0";

function validDetail(): Record<string, unknown> {
  return {
    slug: "cordis-market",
    name: "Cordis Market",
    entryRevision: "2026-08-19T00:00:00.000Z-deadbeef",
    description: { zh: "中文说明", en: "English description" },
    source: {
      type: "npm",
      packageName: "@cordis-mp/web",
      version: "0.1.0",
      integrity: INTEGRITY,
      registry: "https://registry.npmjs.org",
      tarball: "https://registry.npmjs.org/@cordis-mp/web/-/web-0.1.0.tgz",
    },
    platforms: ["web", "desktop"],
    engines: { dsh: CANDIDATE_DSH_ENGINE },
    blocked: false,
    deprecated: false,
  };
}

test("Cordis market URLs remain constrained to desktop production API", () => {
  assert.equal(isValidCordisMarketSlug("cordis-market"), true);
  assert.equal(isValidCordisMarketSlug(MISSING_MARKET_SLUG_PREFIX), true);
  assert.equal(
    isValidCordisMarketSlug(`${MISSING_MARKET_SLUG_PREFIX}-00000000-0000-4000-8000-000000000000`),
    true,
  );
  assert.equal(isValidCordisMarketSlug("../escape"), false);
  assert.equal(isValidCordisMarketSlug("Cordis"), false);
  assert.equal(isValidCordisMarketSlug("a?b"), false);
  assert.equal(
    marketListUrl(1).toString(),
    `${CORDIS_MARKET_API_ORIGIN}/plugins?platform=desktop&limit=1`,
  );
  assert.equal(
    marketDetailUrl("cordis-market").toString(),
    `${CORDIS_MARKET_API_ORIGIN}/plugins/cordis-market`,
  );
  assert.throws(() => marketListUrl(0), /1\.\.100/);
  assert.throws(() => marketDetailUrl("../escape"), /invalid Cordis market slug/);
});

test("Cordis market response headers reject redirects and non-JSON bodies", () => {
  const endpoint = marketListUrl(1);
  assert.equal(isJsonContentType("application/json; charset=utf-8"), true);
  assert.equal(isJsonContentType("text/html"), false);
  assert.equal(
    directJsonResponseProblem({
      endpoint,
      status: 200,
      statusText: "OK",
      contentType: "application/json; charset=utf-8",
      location: null,
    }),
    null,
  );
  assert.match(
    directJsonResponseProblem({
      endpoint,
      status: 307,
      statusText: "Temporary Redirect",
      contentType: null,
      location: "https://cdn.example/catalog",
    }) ?? "",
    /got 307.*Location/,
  );
  assert.match(
    directJsonErrorResponseProblem({
      endpoint: marketDetailUrl(`${MISSING_MARKET_SLUG_PREFIX}-missing`),
      status: 404,
      statusText: "Not Found",
      contentType: "application/json",
      location: null,
      expectedStatus: 404,
    }) ?? "",
    /^$/,
  );
  assert.equal(
    notModifiedResponseProblem({ endpoint, status: 304, statusText: "Not Modified", location: null }),
    null,
  );
});

test("Cordis list and JSON 404 retain the v4 response shape", () => {
  const body = {
    schemaVersion: 1,
    catalogRevision: "revision-1",
    updated: "2026-08-19T00:00:00.000Z",
    count: 0,
    categories: {},
    items: [],
    page: { cursor: null, hasMore: false, limit: 1 },
  };
  assert.equal(marketListResponseProblem(body), null);
  assert.equal(marketListResponseProblem({ ...body, page: { ...body.page, limit: 101 } }), "market list page.limit must be an integer in 1..100");
  assert.equal(marketErrorResponseProblem({ error: { code: "NOT_FOUND", message: "missing" } }, "NOT_FOUND"), null);
  assert.match(marketErrorResponseProblem({ error: { code: "OTHER", message: "missing" } }, "NOT_FOUND") ?? "", /NOT_FOUND/);
});

test("optional real-plugin probe shape matches Desktop's strict nested-source boundary", () => {
  assert.equal(desktopInstallWireProblem(validDetail()), null);
  assert.equal(
    desktopInstallWireProblem({ ...validDetail(), engines: { dsh: ">=0.1.0-rc.6 <0.2.0" } }),
    null,
  );
  const malformedIntegrity = validDetail();
  assert.match(
    desktopInstallWireProblem({
      ...malformedIntegrity,
      source: { ...(malformedIntegrity.source as Record<string, unknown>), integrity: "sha256-bad" },
    }) ?? "",
    /canonical sha512/,
  );
  assert.match(
    desktopInstallWireProblem({ ...validDetail(), platforms: ["web"] }) ?? "",
    /include desktop/,
  );
  assert.match(
    desktopInstallWireProblem({ ...validDetail(), engines: { dsh: "^0.2.0" } }) ?? "",
    /engines\.dsh must be a standard/,
  );
});
