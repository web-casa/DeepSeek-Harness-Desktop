import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  BUNDLE_SPECS,
  NATIVE_RELEASE_TARGETS,
  STORE_MSIX_TARGETS,
  githubNativeMatrix,
  githubMacosNotarizationMatrix,
  githubMsixMatrix,
  releasePlanProblems,
} from "./release-artifacts.ts";

test("release plan covers every requested public format exactly through reviewed targets", () => {
  assert.deepEqual(releasePlanProblems(), []);
  const formats = new Set(NATIVE_RELEASE_TARGETS.flatMap((target) => target.bundles));
  assert.deepEqual([...formats].sort(), Object.keys(BUNDLE_SPECS).sort());
  assert.equal(formats.has("flatpak"), true);
  assert.equal([...formats].includes("msix" as never), false);
});

test("native matrix uses current architecture-specific hosted runners", () => {
  const rows = githubNativeMatrix().include;
  assert.equal(rows.length, 5);
  assert.equal(rows.some((row) => row.os === "macos-14"), false);
  assert.equal(rows.some((row) => row.os === "macos-15-intel"), true);
  assert.equal(rows.some((row) => row.os === "ubuntu-22.04-arm"), true);
  for (const row of rows.filter((candidate) =>
    String(candidate.target).startsWith("macos-"),
  )) {
    assert.equal(row.tauriBundles, "dmg,app");
  }
});

test("macOS notarization handoff is private and derived from the native matrix", () => {
  const rows = githubMacosNotarizationMatrix().include;
  assert.deepEqual(
    rows.map((row) => row.target),
    ["macos-arm64", "macos-x64"],
  );
  for (const row of rows) {
    assert.match(String(row.handoffArtifact), /^dsh-macos-notarization-/);
    assert.doesNotMatch(String(row.handoffArtifact), /^deepseek-harness-desktop-/);
    assert.match(String(row.artifact), /^deepseek-harness-desktop-macos-/);
  }
});

test("Store MSIX remains a separate exact x64 and arm64 matrix", () => {
  assert.deepEqual(
    STORE_MSIX_TARGETS.map((target) => target.arch),
    ["x64", "arm64"],
  );
  assert.equal(
    NATIVE_RELEASE_TARGETS.some((target) => target.artifact.includes("store-msix")),
    false,
  );
  assert.deepEqual(
    githubMsixMatrix().include.map((target) => target.artifact),
    ["dsh-desktop-store-msix-x64", "dsh-desktop-store-msix-arm64"],
  );
});

test("every public artifact path includes both installer and SHA-256 sidecar", () => {
  for (const target of NATIVE_RELEASE_TARGETS) {
    for (const bundle of target.bundles) {
      const spec = BUNDLE_SPECS[bundle];
      assert.equal(
        target.uploadPaths.includes(`${spec.directory}/*${spec.suffix}`),
        true,
      );
      assert.equal(
        target.uploadPaths.includes(`${spec.directory}/*${spec.suffix}.sha256`),
        true,
      );
    }
  }
});

test("both reusable quality jobs checkout the requested release revision", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/quality.yml", import.meta.url),
    "utf8",
  );
  const exactRef = "ref: ${{ inputs.checkout_ref || github.ref }}";
  assert.equal(workflow.split(exactRef).length - 1, 2);
});

test("Windows installer smoke searches the preserved artifact tree exactly", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(
    workflow,
    /Get-ChildItem -Path artifacts -Recurse -File -Filter '\*-setup\.exe'/,
  );
  assert.match(
    workflow,
    /Get-ChildItem -Path artifacts -Recurse -File -Filter '\*\.msi'/,
  );
  assert.equal(workflow.split("$installers.Count -ne 1").length - 1, 2);
  assert.match(workflow, /\$quotedInstaller = '\"' \+ \$installer\.FullName \+ '\"'/);
  assert.match(workflow, /\/l\*v \$quotedLogPath/);
  assert.match(workflow, /\$process\.WaitForExit\(600000\)/);
});

test("Windows installer smoke can safely reuse one completed Release run", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(workflow, /windows_smoke_source_run_id:/);
  assert.match(workflow, /actions: read/);
  assert.match(
    workflow,
    /source_workflow" != "\.github\/workflows\/release\.yml"/,
  );
  assert.match(
    workflow,
    /source_status" != "completed"/,
  );
  assert.match(
    workflow,
    /select\(\.name == "deepseek-harness-desktop-windows-x64" and \.expired == false\)/,
  );
  const sourceRun =
    "run-id: ${{ github.event.inputs.windows_smoke_source_run_id || github.run_id }}";
  assert.equal(workflow.split(sourceRun).length - 1, 2);
  assert.equal(
    workflow.split("needs: [build, windows-smoke-source]").length - 1,
    2,
  );
  assert.equal(
    workflow.split(
      "if: github.event_name != 'workflow_dispatch' || github.event.inputs.windows_smoke_source_run_id == ''",
    ).length - 1,
    3,
  );
});

test("macOS release signs runtime, uploads once, then waits in a separate job", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  const importIndex = workflow.indexOf("run: node scripts/import-apple-certificate.ts");
  const nestedIndex = workflow.indexOf("run: node scripts/sign-macos-runtime.ts");
  const postSignSmokeIndex = workflow.indexOf("name: Runtime smoke (post-sign)");
  const bundleIndex = workflow.indexOf("name: Bundle application (macOS, signed; notarization deferred)");
  const preVerifyIndex = workflow.indexOf("name: Verify signed DMG before notarization");
  const preSubmitCleanupIndex = workflow.indexOf(
    "name: Remove job-scoped macOS signing keychain before submission",
  );
  const submitIndex = workflow.indexOf("name: Submit signed DMG without waiting");
  const handoffIndex = workflow.indexOf("name: Upload notarization handoff");
  const waitJobIndex = workflow.indexOf("notarize-macos:");
  const waitIndex = workflow.indexOf("name: Wait for the recorded Apple submission");
  const windowsIndex = workflow.indexOf("name: Bundle application (Windows)");
  const cleanupIndex = workflow.indexOf("name: Remove remaining job-scoped macOS signing keychain");
  assert.equal(importIndex > 0, true);
  assert.equal(nestedIndex > importIndex, true);
  assert.equal(postSignSmokeIndex > nestedIndex, true);
  assert.equal(bundleIndex > postSignSmokeIndex, true);
  assert.equal(preVerifyIndex > bundleIndex, true);
  assert.equal(preSubmitCleanupIndex > preVerifyIndex, true);
  assert.equal(submitIndex > preSubmitCleanupIndex, true);
  assert.equal(submitIndex > bundleIndex, true);
  assert.equal(handoffIndex > submitIndex, true);
  assert.equal(waitJobIndex > handoffIndex, true);
  assert.equal(waitIndex > waitJobIndex, true);
  assert.equal(cleanupIndex > bundleIndex, true);
  assert.match(workflow, /run: node scripts\/macos-notarization\.ts submit/);
  assert.match(workflow, /run: node scripts\/macos-notarization\.ts wait/);

  // Tauri must use the already-imported identity. Passing the PKCS#12 again
  // creates a separate process-scoped keychain that the nested signer cannot
  // access and makes the two signing paths needlessly diverge.
  const signedBundleStep = workflow.slice(bundleIndex, preVerifyIndex);
  assert.equal(signedBundleStep.includes("APPLE_CERTIFICATE:"), false);
  assert.equal(signedBundleStep.includes("APPLE_ID:"), false);
  assert.equal(signedBundleStep.includes("APPLE_PASSWORD:"), false);
  const importer = readFileSync(
    new URL("../import-apple-certificate.ts", import.meta.url),
    "utf8",
  );
  assert.equal(importer.includes("APPLE_SIGNING_IDENTITY=${identity}"), true);
  assert.equal(workflow.slice(cleanupIndex).includes("always()"), true);
  assert.equal(workflow.slice(cleanupIndex).includes("::warning::"), true);
});
