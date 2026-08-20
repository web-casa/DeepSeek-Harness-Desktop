import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  BUNDLE_SPECS,
  NATIVE_RELEASE_TARGETS,
  STORE_MSIX_TARGETS,
  githubNativeMatrix,
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

test("macOS release signs arbitrary runtime code before the outer app", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  const importIndex = workflow.indexOf("run: node scripts/import-apple-certificate.ts");
  const nestedIndex = workflow.indexOf("run: node scripts/sign-macos-runtime.ts");
  const postSignSmokeIndex = workflow.indexOf("name: Runtime smoke (post-sign)");
  const bundleIndex = workflow.indexOf("name: Bundle application (macOS, signed and notarized)");
  const windowsIndex = workflow.indexOf("name: Bundle application (Windows)");
  const cleanupIndex = workflow.indexOf("name: Remove job-scoped macOS signing keychain");
  assert.equal(importIndex > 0, true);
  assert.equal(nestedIndex > importIndex, true);
  assert.equal(postSignSmokeIndex > nestedIndex, true);
  assert.equal(bundleIndex > postSignSmokeIndex, true);
  assert.equal(cleanupIndex > bundleIndex, true);

  // Tauri must use the already-imported identity. Passing the PKCS#12 again
  // creates a separate process-scoped keychain that the nested signer cannot
  // access and makes the two signing paths needlessly diverge.
  const signedBundleStep = workflow.slice(bundleIndex, windowsIndex);
  assert.equal(signedBundleStep.includes("APPLE_CERTIFICATE:"), false);
  const importer = readFileSync(
    new URL("../import-apple-certificate.ts", import.meta.url),
    "utf8",
  );
  assert.equal(importer.includes("APPLE_SIGNING_IDENTITY=${identity}"), true);
  assert.equal(workflow.slice(cleanupIndex).includes("always()"), true);
});
