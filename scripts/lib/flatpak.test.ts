import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { repoRoot } from "./common.ts";
import {
  FLATPAK_ID,
  FLATPAK_RUNTIME_VERSION,
  flatpakArch,
  flatpakContractProblems,
  flatpakManifest,
  flatpakMetadataProblems,
} from "./flatpak.ts";

test("Flatpak manifest stays pinned and minimally permissioned", () => {
  assert.deepEqual(flatpakContractProblems(), []);
  const manifest = flatpakManifest();
  assert.equal(manifest["app-id"], FLATPAK_ID);
  assert.equal(manifest["runtime-version"], FLATPAK_RUNTIME_VERSION);
  assert.equal(manifest.branch, "stable");
  assert.deepEqual(manifest["build-options"], {
    strip: false,
    "no-debuginfo": true,
  });
  assert.equal(JSON.stringify(manifest).includes("--filesystem=host"), false);
  assert.equal(JSON.stringify(manifest).includes("--socket=session-bus"), false);
});

test("committed AppStream metadata matches the Flatpak identity", () => {
  const metadata = readFileSync(
    join(repoRoot, "packaging", "flatpak", `${FLATPAK_ID}.metainfo.xml`),
    "utf8",
  );
  assert.deepEqual(flatpakMetadataProblems(metadata), []);
  assert.notDeepEqual(flatpakMetadataProblems(metadata.replace(FLATPAK_ID, "invalid")), []);
});

test("Flatpak uses native architecture vocabulary", () => {
  assert.equal(flatpakArch("x64"), "x86_64");
  assert.equal(flatpakArch("arm64"), "aarch64");
});
