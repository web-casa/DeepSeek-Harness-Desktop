import { test } from "node:test";
import assert from "node:assert/strict";
import {
  classifyPublicInstaller,
  expectedPublicBundleCounts,
  expectedUpdaterSignatureCount,
} from "./release-inventory.ts";

test("release filenames classify without treating sidecars as installers", () => {
  assert.equal(classifyPublicInstaller("DSH Desktop_0.2.8_x64-setup.exe"), "nsis");
  assert.equal(classifyPublicInstaller("DSH Desktop_0.2.8_x64_en-US.msi"), "msi");
  assert.equal(classifyPublicInstaller("DSH Desktop_0.2.8_aarch64.AppImage"), "appimage");
  assert.equal(classifyPublicInstaller("DSH Desktop_0.2.8_arm64.deb.sha256"), null);
  assert.equal(classifyPublicInstaller("unsigned.msix"), null);
});

test("release inventory expects every architecture row and updater tripwire", () => {
  assert.deepEqual(expectedPublicBundleCounts(), {
    nsis: 1,
    msi: 1,
    dmg: 2,
    appimage: 2,
    deb: 2,
    rpm: 2,
    flatpak: 2,
  });
  assert.equal(expectedUpdaterSignatureCount(), 1);
});
