import assert from "node:assert/strict";
import { test } from "node:test";
import {
  CORDIS_PRESET_ORIGIN,
  directZipResponseProblem,
  isValidCordisPresetSlug,
  isZipContentType,
  presetDownloadUrl,
} from "./cordis-preset-contract.ts";

test("Cordis preset slug and canonical download URL stay constrained", () => {
  assert.equal(isValidCordisPresetSlug("code"), true);
  assert.equal(isValidCordisPresetSlug("v2-demo"), true);
  assert.equal(isValidCordisPresetSlug("-code"), false);
  assert.equal(isValidCordisPresetSlug("Code"), false);
  assert.equal(isValidCordisPresetSlug("code/payload"), false);
  assert.equal(isValidCordisPresetSlug("code?next=x"), false);

  assert.equal(
    presetDownloadUrl("v2-demo").toString(),
    `${CORDIS_PRESET_ORIGIN}/api/presets/v2-demo/download`,
  );
  assert.throws(() => presetDownloadUrl("../other"), /invalid Cordis preset slug/);
});

test("Cordis preset response requires direct ZIP semantics", () => {
  const endpoint = presetDownloadUrl("code");
  assert.equal(isZipContentType("application/zip"), true);
  assert.equal(isZipContentType("Application/Zip; charset=binary"), true);
  assert.equal(isZipContentType("application/octet-stream"), false);
  assert.equal(isZipContentType(null), false);

  assert.equal(
    directZipResponseProblem({
      endpoint,
      status: 200,
      statusText: "OK",
      contentType: "application/zip",
      location: null,
    }),
    null,
  );
  assert.match(
    directZipResponseProblem({
      endpoint,
      status: 307,
      statusText: "Temporary Redirect",
      contentType: null,
      location: "https://cdn.example/preset.dshpreset",
    }) ?? "",
    /redirects are rejected/,
  );
  assert.match(
    directZipResponseProblem({
      endpoint,
      status: 200,
      statusText: "OK",
      contentType: "text/html",
      location: null,
    }) ?? "",
    /Content-Type application\/zip/,
  );
  assert.match(
    directZipResponseProblem({
      endpoint,
      status: 200,
      statusText: "OK",
      contentType: "application/zip",
      location: "https://cdn.example/preset.dshpreset",
    }) ?? "",
    /no redirect Location/,
  );
});
