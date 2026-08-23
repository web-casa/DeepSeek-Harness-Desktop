import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  WINDOWS_NSIS_INSTALLER_LANGUAGES,
  WINDOWS_WIX_INSTALLER_LOCALES,
  WINDOWS_WIX_PRODUCT_LANGUAGE,
  isWindowsWixInstallerLocale,
  wixInstallerLocaleFromMsiName,
} from "./windows-installer-locales.ts";

test("Windows installer language contract is explicit and complete", () => {
  assert.deepEqual(WINDOWS_WIX_INSTALLER_LOCALES, ["en-US", "zh-CN"]);
  assert.deepEqual(WINDOWS_NSIS_INSTALLER_LANGUAGES, ["English", "SimpChinese"]);
  assert.deepEqual(WINDOWS_WIX_PRODUCT_LANGUAGE, { "en-US": 1033, "zh-CN": 2052 });
  assert.equal(isWindowsWixInstallerLocale("zh-CN"), true);
  assert.equal(isWindowsWixInstallerLocale("zh-TW"), false);
});

test("MSI locale is accepted only when it is the final, exact WiX suffix", () => {
  assert.equal(
    wixInstallerLocaleFromMsiName("DSH Desktop_0.2.14_x64_en-US.msi"),
    "en-US",
  );
  assert.equal(
    wixInstallerLocaleFromMsiName("DSH Desktop_0.2.14_arm64_zh-CN.msi"),
    "zh-CN",
  );
  assert.equal(wixInstallerLocaleFromMsiName("DSH Desktop_0.2.14_x64.msi"), null);
  assert.equal(wixInstallerLocaleFromMsiName("DSH Desktop_0.2.14_x64_zh-CN.msi.sha256"), null);
  assert.equal(wixInstallerLocaleFromMsiName("DSH Desktop_0.2.14_x64_zh-TW.msi"), null);
});

test("Tauri config emits the reviewed WiX locales and one multilingual NSIS installer", () => {
  const config = JSON.parse(
    readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  ) as {
    bundle?: {
      windows?: {
        webviewInstallMode?: { type?: string };
        wix?: { language?: string[] };
        nsis?: { languages?: string[]; displayLanguageSelector?: boolean };
      };
    };
  };
  assert.deepEqual(config.bundle?.windows?.wix?.language, WINDOWS_WIX_INSTALLER_LOCALES);
  assert.deepEqual(config.bundle?.windows?.nsis?.languages, WINDOWS_NSIS_INSTALLER_LANGUAGES);
  assert.equal(config.bundle?.windows?.nsis?.displayLanguageSelector, true);
  // Keep the Tauri default explicit: a clean supported Windows installation
  // may need the Evergreen WebView2 bootstrapper before the first launch.
  assert.deepEqual(config.bundle?.windows?.webviewInstallMode, {
    type: "downloadBootstrapper",
  });
});
