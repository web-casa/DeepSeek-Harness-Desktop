// Unit tests for the pure decision/parsing logic (node:test — zero deps).
// Run: node --test scripts/lib/  (or pnpm test:scripts)

import { test } from "node:test";
import assert from "node:assert/strict";
import { expectedSigned, parseAuthenticode, toolRan } from "./signing.ts";
import { quarantinePresent, parseSltListing } from "./bundle-checks.ts";
import { isParseableReadyLine } from "./heartbeat-sim.ts";
import { isPackageRoot } from "./licenses.ts";

test("expectedSigned: platform secret presence decides", () => {
  assert.equal(expectedSigned("dmg", {}), false);
  assert.equal(expectedSigned("dmg", { APPLE_CERTIFICATE: "x" }), true);
  assert.equal(expectedSigned("dmg", { APPLE_CERTIFICATE: "" }), false);
  assert.equal(expectedSigned("nsis", {}), false);
  assert.equal(expectedSigned("nsis", { WINDOWS_CERTIFICATE: "x" }), true);
  // The other platform's secret must not leak across.
  assert.equal(expectedSigned("dmg", { WINDOWS_CERTIFICATE: "x" }), false);
  assert.equal(expectedSigned("nsis", { APPLE_CERTIFICATE: "x" }), false);
});

test("parseAuthenticode: first non-empty trimmed line, CRLF tolerant", () => {
  assert.equal(parseAuthenticode("Valid\r\n"), "Valid");
  assert.equal(parseAuthenticode("  NotSigned\r\n"), "NotSigned");
  assert.equal(parseAuthenticode("HashMismatch\n"), "HashMismatch");
  assert.equal(parseAuthenticode("\r\n"), null);
  assert.equal(parseAuthenticode(""), null);
});

test("toolRan: null status / spawn error must never read as a result", () => {
  assert.equal(toolRan({ status: 0 }), true);
  assert.equal(toolRan({ status: 1 }), true);
  assert.equal(toolRan({ status: null }), false);
  assert.equal(toolRan({ status: null, error: new Error("ENOENT") }), false);
});

test("quarantinePresent: clean/absent/indeterminate", () => {
  assert.equal(quarantinePresent(0), true);
  assert.equal(quarantinePresent(1), false);
  assert.equal(quarantinePresent(null), null);
  assert.equal(quarantinePresent(null, new Error("ENOENT")), null);
});

test("parseSltListing: header block excluded, paths normalized", () => {
  const fixture = [
    "7-Zip 24.09 : Copyright",
    "",
    "Listing archive: installer.exe",
    "--",
    "Path = D:\\a\\repo\\installer.exe",
    "----------",
    "Path = DSH Desktop.exe",
    "Path = runtime\\node.exe",
    "Path = runtime/harness/a b/bin.js",
  ].join("\n");
  assert.deepEqual(parseSltListing(fixture), [
    "DSH Desktop.exe",
    "runtime/node.exe",
    "runtime/harness/a b/bin.js",
  ]);
});

test("isParseableReadyLine: exact sidecar contract shape", () => {
  assert.equal(isParseableReadyLine("dsh web: http://127.0.0.1:49321"), true);
  assert.equal(isParseableReadyLine("dsh web: http://127.0.0.1:1"), true);
  assert.equal(isParseableReadyLine("dsh web: http://127.0.0.1:0"), false);
  assert.equal(isParseableReadyLine("dsh web: http://127.0.0.1:123abc"), false);
  assert.equal(isParseableReadyLine("dsh web: http://127.0.0.1:70000"), false);
  assert.equal(isParseableReadyLine("dsh web: http://127.0.0.1:65535"), true);
  assert.equal(isParseableReadyLine("dsh web: http://192.168.1.5:49321"), false);
  assert.equal(isParseableReadyLine("dsh web: http://127.0.0.1:49321 (LAN: …)"), false);
  assert.equal(isParseableReadyLine(""), false);
});

test("isPackageRoot: only real package shapes count", () => {
  assert.equal(isPackageRoot(["sharp"]), true);
  assert.equal(isPackageRoot(["@aws-sdk", "client-s3"]), true);
  assert.equal(isPackageRoot(["a", "node_modules", "b"]), true);
  assert.equal(isPackageRoot(["a", "node_modules", "@scope", "b"]), true);
  // The {"type":"module"} stubs in dist/esm etc. are NOT packages.
  assert.equal(isPackageRoot(["@babel", "runtime", "helpers", "esm"]), false);
  assert.equal(isPackageRoot(["hono", "dist", "cjs"]), false);
  assert.equal(isPackageRoot([]), false);
  // A scope directory itself carries no package.json in practice, but even
  // so it must not count as a package root at depth 1.
  assert.equal(isPackageRoot(["@scope", "pkg", "sub"]), false);
});
