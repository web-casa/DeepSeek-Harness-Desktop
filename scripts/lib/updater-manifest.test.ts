import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  assembleLatestJson,
  githubReleaseAssetUrl,
  isWindowsNsisUpdaterPlatform,
  platformArtifactFor,
} from "./updater-manifest.ts";
import { publishedWindowsNsisUpdaterPlatforms } from "./release-artifacts.ts";
import { repoRoot } from "./common.ts";

const assets = [
  { id: 1, name: "DSH Desktop_0.2.14_x64-setup.exe" },
  { id: 2, name: "DSH Desktop_0.2.14_x64-setup.exe.sig" },
  { id: 3, name: "DSH Desktop_0.2.14_arm64-setup.exe" },
  { id: 4, name: "DSH Desktop_0.2.14_arm64-setup.exe.sig" },
  { id: 5, name: "DSH Desktop_0.2.14_x64_en-US.msi" },
] as const;

test("updater manifest pairs each exact NSIS architecture, never the first setup executable", () => {
  assert.equal(
    platformArtifactFor(assets, "windows-x86_64-nsis")?.artifact.name,
    "DSH Desktop_0.2.14_x64-setup.exe",
  );
  assert.equal(
    platformArtifactFor(assets, "windows-aarch64-nsis")?.artifact.name,
    "DSH Desktop_0.2.14_arm64-setup.exe",
  );
});

test("updater manifest does not expose a generic or MSI target", () => {
  assert.equal(isWindowsNsisUpdaterPlatform("windows-x86_64"), false);
  assert.equal(isWindowsNsisUpdaterPlatform("windows-x86_64-msi"), false);
  assert.equal(platformArtifactFor([{ id: 1, name: "DSH Desktop_0.2.14_x64_en-US.msi" }], "windows-x86_64-nsis"), null);
});

test("published updater platforms follow the reviewed native release plan", () => {
  assert.deepEqual(publishedWindowsNsisUpdaterPlatforms(), [
    "windows-x86_64-nsis",
    "windows-aarch64-nsis",
  ]);
  const doc = JSON.parse(
    assembleLatestJson("0.2.14", "notes", "2026-08-22T00:00:00Z", {
      "windows-x86_64-nsis": { signature: "sig-x64", url: "https://example.test/x64.exe" },
      "windows-aarch64-nsis": { signature: "sig-arm64", url: "https://example.test/arm64.exe" },
    }),
  ) as { platforms: Record<string, { signature: string }> };
  assert.deepEqual(Object.keys(doc.platforms), ["windows-x86_64-nsis", "windows-aarch64-nsis"]);
  assert.equal(doc.platforms["windows-aarch64-nsis"].signature, "sig-arm64");
});

test("updater manifest percent-encodes the exact asset filename as one URL segment", () => {
  assert.equal(
    githubReleaseAssetUrl(
      "web-casa/DeepSeek-Harness-Desktop",
      "v0.2.14",
      "DSH Desktop_0.2.14_x64-setup.exe",
    ),
    "https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/download/v0.2.14/DSH%20Desktop_0.2.14_x64-setup.exe",
  );
  assert.throws(
    () => githubReleaseAssetUrl("web-casa/DeepSeek-Harness-Desktop", "v0.2.14", "../wrong.exe"),
    /invalid GitHub release asset identity/,
  );
});

test("Desktop updater pins the same exact target keys and performs lifecycle handoff", () => {
  const commands = readFileSync(`${repoRoot}/src-tauri/src/commands.rs`, "utf8");
  for (const platform of publishedWindowsNsisUpdaterPlatforms()) {
    assert.ok(commands.includes(`"${platform}"`), `missing Desktop updater target ${platform}`);
  }
  assert.match(commands, /\.updater_builder\(\)\s*\.target\(target\)/);
  assert.match(commands, /\.timeout\(UPDATE_CHECK_TIMEOUT\)/);
  assert.match(commands, /\.timeout\(UPDATE_DOWNLOAD_TIMEOUT\)/);
  assert.match(commands, /\.on_before_exit\(move \|\| prepare_for_updater_exit\(&app_before_exit\)\)/);
  assert.match(commands, /crate::harness::shutdown_blocking\(app\)/);
});
