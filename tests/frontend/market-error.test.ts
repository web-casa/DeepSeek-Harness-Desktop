import assert from "node:assert/strict";
import { test } from "node:test";
import { classifyMarketFailure, marketFailureText } from "../../src/lib/market-error.ts";

const context = "市场搜索失败";

test("market error categories have stable user-facing messages", () => {
  assert.equal(
    marketFailureText(context, "MARKET_TIMEOUT: market search request timed out"),
    "市场搜索失败：请求超时，请检查网络后重试",
  );
  assert.equal(
    marketFailureText(context, "MARKET_UNAVAILABLE: market search is unavailable"),
    "市场搜索失败：市场服务暂时不可用，请稍后重试",
  );
  assert.equal(
    marketFailureText(context, "MARKET_INVALID_RESPONSE: market search invalid JSON"),
    "市场搜索失败：市场服务返回了无法识别的数据",
  );
  assert.equal(
    marketFailureText(context, "MARKET_HTTP_ERROR: market search failed with HTTP 502"),
    "市场搜索失败：市场服务返回 HTTP 错误",
  );
});

test("bounded API detail is retained but unknown error text is never reflected", () => {
  assert.equal(
    marketFailureText(
      context,
      "MARKET_API_ERROR: market detail failed: 404 Not Found NOT_FOUND: no such slug (requestId: req-1)",
    ),
    "市场搜索失败：market detail failed: 404 Not Found NOT_FOUND: no such slug (requestId: req-1)",
  );
  assert.equal(
    marketFailureText(context, "MARKET_API_ERROR:"),
    "市场搜索失败：市场服务返回错误",
  );
  assert.equal(
    marketFailureText(context, "socket failed at https://secret.invalid/?token=not-for-ui"),
    "市场搜索失败：市场请求失败，请稍后重试",
  );
});

test("classification keeps only the bounded API detail for the localized controller", () => {
  assert.deepEqual(
    classifyMarketFailure("MARKET_API_ERROR: NOT_FOUND: no such slug (requestId: req-1)"),
    { kind: "api", detail: "NOT_FOUND: no such slug (requestId: req-1)" },
  );
  assert.deepEqual(classifyMarketFailure("MARKET_API_ERROR:"), { kind: "api" });
  assert.deepEqual(
    classifyMarketFailure("socket failed at https://secret.invalid/?token=not-for-ui"),
    { kind: "unknown" },
  );
});
