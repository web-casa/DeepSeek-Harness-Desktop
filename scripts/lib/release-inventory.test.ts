import { test } from "node:test";
import assert from "node:assert/strict";
import {
  classifyPublicInstaller,
  expectedPublicBundleCounts,
  expectedMsiInstallerLocaleCounts,
  msiLocaleInventoryProblems,
  expectedUpdaterSignatureCount,
} from "./release-inventory.ts";

test("release filenames classify without treating sidecars as installers", () => {
  assert.equal(classifyPublicInstaller("DSH Desktop_0.2.8_x64-setup.exe"), "nsis");
  assert.equal(classifyPublicInstaller("DSH Desktop_0.2.8_x64_en-US.msi"), "msi");
  assert.equal(classifyPublicInstaller("DSH Desktop_0.2.8_aarch64.AppImage"), "appimage");
  assert.equal(classifyPublicInstaller("DSH Desktop_0.2.8_arm64.deb.sha256"), null);
  assert.equal(classifyPublicInstaller("unsigned.msix"), null);
});

test("release inventory expects every architecture row and public updater signature", () => {
  assert.deepEqual(expectedPublicBundleCounts(), {
    nsis: 2,
    msi: 4,
    dmg: 2,
    appimage: 2,
    deb: 2,
    rpm: 2,
    flatpak: 2,
  });
  assert.deepEqual(expectedMsiInstallerLocaleCounts(), {
    "en-US": 2,
    "zh-CN": 2,
  });
  assert.equal(expectedUpdaterSignatureCount(), 2);
});

test("release inventory rejects a cosmetic or imbalanced MSI language suffix", () => {
  const complete = [
    "DSH Desktop_0.2.14_x64_en-US.msi",
    "DSH Desktop_0.2.14_x64_zh-CN.msi",
    "DSH Desktop_0.2.14_arm64_en-US.msi",
    "DSH Desktop_0.2.14_arm64_zh-CN.msi",
  ];
  assert.deepEqual(msiLocaleInventoryProblems(complete), []);
  assert.deepEqual(
    msiLocaleInventoryProblems([
      ...complete.slice(0, 3),
      "DSH Desktop_0.2.14_arm64_en-US.msi",
    ]),
    ["release inventory MSI en-US count 3 != expected 2", "release inventory MSI zh-CN count 1 != expected 2"],
  );
  assert.deepEqual(
    msiLocaleInventoryProblems([
      ...complete.slice(0, 3),
      "DSH Desktop_0.2.14_arm64.msi",
    ]),
    [
      "MSI filename lacks a reviewed WiX locale suffix: DSH Desktop_0.2.14_arm64.msi",
      "release inventory MSI zh-CN count 1 != expected 2",
    ],
  );
});
