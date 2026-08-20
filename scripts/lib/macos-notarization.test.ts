import test from "node:test";
import assert from "node:assert/strict";
import {
  isRetryableNotarizationError,
  isTerminalNotarizationAuthError,
  extractSubmissionId,
  normalizeNotarizationStatus,
  notarizationPollBackoff,
  notarizationStateProblems,
  parseNotarizationResponse,
  parseNotarizationSubmissionResponse,
  resolveMacosSigningConfiguration,
} from "./macos-notarization.ts";

const baseSigning = {
  APPLE_CERTIFICATE: "p12",
  APPLE_CERTIFICATE_PASSWORD: "secret",
  APPLE_TEAM_ID: "ABCDE12345",
};

test("macOS signing configuration prefers complete ASC auth and preserves Apple-ID fallback", () => {
  assert.deepEqual(resolveMacosSigningConfiguration({}), {
    configured: false,
    provider: null,
    problems: [],
  });
  assert.deepEqual(
    resolveMacosSigningConfiguration({
      ...baseSigning,
      APPLE_ID: "developer@example.com",
      APPLE_PASSWORD: "app-password",
    }),
    { configured: true, provider: "notarytool", problems: [] },
  );
  assert.deepEqual(
    resolveMacosSigningConfiguration({
      ...baseSigning,
      APPLE_ID: "developer@example.com",
      APPLE_PASSWORD: "app-password",
      ASC_KEY_ID: "KEY123",
      ASC_ISSUER_ID: "issuer",
      ASC_PRIVATE_KEY_B64: "base64",
    }),
    { configured: true, provider: "asc", problems: [] },
  );
});

test("macOS signing configuration rejects every partial secret set", () => {
  assert.match(
    resolveMacosSigningConfiguration({ APPLE_ID: "developer@example.com" }).problems.join("\n"),
    /APPLE_CERTIFICATE is missing/,
  );
  assert.match(
    resolveMacosSigningConfiguration({ ...baseSigning, APPLE_ID: "developer@example.com" }).problems.join(
      "\n",
    ),
    /configured together/,
  );
  assert.match(
    resolveMacosSigningConfiguration({ ...baseSigning, ASC_KEY_ID: "KEY123" }).problems.join("\n"),
    /ASC_KEY_ID, ASC_ISSUER_ID and ASC_PRIVATE_KEY_B64/,
  );
});

test("normalizes and parses ASC and notarytool responses", () => {
  const id = "d66bbcf0-7a30-4bfa-b635-eca5774c8c5b";
  assert.equal(normalizeNotarizationStatus("in_progress"), "In Progress");
  assert.equal(normalizeNotarizationStatus("InProgress"), "In Progress");
  assert.deepEqual(
    parseNotarizationResponse(
      "asc",
      JSON.stringify({ data: { id, attributes: { status: "In Progress" } } }),
    ),
    { submissionId: id, status: "In Progress" },
  );
  assert.deepEqual(
    parseNotarizationResponse("notarytool", JSON.stringify({ id, status: "Accepted" }), id),
    { submissionId: id, status: "Accepted" },
  );
  assert.deepEqual(
    parseNotarizationSubmissionResponse(
      "notarytool",
      JSON.stringify({ id, message: "Successfully uploaded file", path: "/tmp/App.dmg" }),
    ),
    { submissionId: id, status: "In Progress" },
  );
  assert.deepEqual(
    parseNotarizationSubmissionResponse(
      "notarytool",
      JSON.stringify({ id, status: "Uploaded", message: "Successfully uploaded file" }),
    ),
    { submissionId: id, status: "In Progress" },
  );
  assert.throws(
    () => parseNotarizationResponse("notarytool", JSON.stringify({ id, status: "Mystery" })),
    /unknown notarization status/,
  );
});

test("validates persisted notarization state as an untrusted artifact", () => {
  const state = {
    schemaVersion: 1,
    target: "macos-arm64",
    arch: "arm64",
    provider: "asc",
    submissionId: "d66bbcf0-7a30-4bfa-b635-eca5774c8c5b",
    artifactName: "DSH Desktop_0.2.8_aarch64.dmg",
    artifactSha256: "a".repeat(64),
    status: "In Progress",
    createdAt: "2026-08-20T12:00:00.000Z",
  };
  assert.deepEqual(notarizationStateProblems(state, "macos-arm64"), []);
  assert.match(
    notarizationStateProblems({ ...state, artifactName: "../escape.dmg" }).join("\n"),
    /artifactName/,
  );
  assert.match(
    notarizationStateProblems({ ...state, target: "macos-x64" }, "macos-arm64").join("\n"),
    /target/,
  );
});

test("retries transport/server failures but not authentication failures", () => {
  assert.equal(
    isRetryableNotarizationError(
      'NSURLErrorDomain Code=-1009 "The Internet connection appears to be offline" (No network route)',
    ),
    true,
  );
  assert.equal(isRetryableNotarizationError("HTTP 503 Service Unavailable"), true);
  assert.equal(isTerminalNotarizationAuthError("HTTP 401 Unauthorized: invalid credentials"), true);
  assert.equal(isRetryableNotarizationError("status Invalid"), false);
  assert.equal(notarizationPollBackoff(30_000, 1), 30_000);
  assert.equal(notarizationPollBackoff(30_000, 3), 120_000);
  assert.equal(notarizationPollBackoff(60_000, 8), 120_000);
});

test("recovers a Submission ID from upload diagnostics before considering a retry", () => {
  const id = "d66bbcf0-7a30-4bfa-b635-eca5774c8c5b";
  assert.equal(extractSubmissionId(`Submission created: ${id}`), id);
  assert.equal(
    extractSubmissionId(`NSErrorFailingURL=https://appstoreconnect.apple.com/notary/v2/submissions/${id}?`),
    id,
  );
  assert.equal(extractSubmissionId("network failed before Apple returned an ID"), null);
  assert.equal(extractSubmissionId("Submission created: ../../escape"), null);
});
