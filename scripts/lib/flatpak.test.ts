import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { repoRoot } from "./common.ts";
import {
  FLATPAK_BRANCH,
  FLATPAK_COMMAND,
  FLATPAK_FINISH_ARGS,
  FLATPAK_ID,
  FLATPAK_RUNTIME_REPO,
  FLATPAK_RUNTIME_VERSION,
  flatpakArch,
  flatpakContractProblems,
  flatpakMetadataProblems,
} from "./flatpak.ts";

test("Flatpak build contract stays pinned and minimally permissioned", () => {
  assert.deepEqual(flatpakContractProblems(), []);
  assert.equal(FLATPAK_ID, "com.yeagoo.dsh-desktop");
  assert.equal(FLATPAK_RUNTIME_VERSION, "49");
  assert.equal(FLATPAK_BRANCH, "stable");
  assert.equal(FLATPAK_COMMAND, "deepseek-harness-desktop");
  assert.equal(FLATPAK_RUNTIME_REPO, "https://dl.flathub.org/repo/flathub.flatpakrepo");
  const finishArgs = FLATPAK_FINISH_ARGS as readonly string[];
  assert.equal(finishArgs.includes("--filesystem=host"), false);
  assert.equal(finishArgs.includes("--socket=session-bus"), false);
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

test("Flatpak runtime and SDK branches are commit-pinned per architecture", () => {
  const installer = readFileSync(
    join(repoRoot, "scripts", "ci", "install-linux-release-deps.sh"),
    "utf8",
  );
  assert.equal(installer.match(/^\s*platform_commit=[a-f0-9]{64}$/gm)?.length, 2);
  assert.equal(installer.match(/^\s*sdk_commit=[a-f0-9]{64}$/gm)?.length, 2);
  assert.match(installer, /actual_platform=.*--show-commit org\.gnome\.Platform\/\/49/);
  assert.match(installer, /actual_sdk=.*--show-commit org\.gnome\.Sdk\/\/49/);
  assert.match(installer, /actual_platform.*!=.*platform_commit/s);
  assert.match(installer, /actual_sdk.*!=.*sdk_commit/s);
});
