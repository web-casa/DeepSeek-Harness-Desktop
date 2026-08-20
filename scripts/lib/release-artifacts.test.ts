import { test } from "node:test";
import assert from "node:assert/strict";
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
