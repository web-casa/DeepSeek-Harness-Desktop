import assert from "node:assert/strict";
import { test } from "node:test";
import {
  CONTROLLER_LOCALE_STORAGE_KEY,
  detectSystemLocale,
  formatControllerDate,
  loadLocalePreference,
  nativePreferenceMigration,
  resolveControllerLocale,
  saveLocalePreference,
  translate,
  type KeyValueStorage,
} from "../../src/lib/controller-i18n.ts";

class MemoryStorage implements KeyValueStorage {
  readonly values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }
}

test("system locale detection supports the shipped Chinese and English variants", () => {
  assert.equal(detectSystemLocale(["fr-FR", "zh_Hans_CN", "en-US"]), "zh-CN");
  assert.equal(detectSystemLocale(["zh-SG"]), "zh-CN");
  assert.equal(detectSystemLocale(["ja-JP", "en-GB", "zh-CN"]), "en");
  assert.equal(detectSystemLocale(["zh-Hant-TW", "fr-FR"]), "en");
  assert.equal(detectSystemLocale([]), "en");
});

test("manual preference overrides system detection and system preference remains live", () => {
  assert.equal(resolveControllerLocale("en", ["zh-CN"]), "en");
  assert.equal(resolveControllerLocale("zh-CN", ["en-US"]), "zh-CN");
  assert.equal(resolveControllerLocale("system", ["zh-Hans"]), "zh-CN");
  assert.equal(resolveControllerLocale("system", ["en-US"]), "en");
});

test("preference persistence accepts only the reviewed enum and fails open", () => {
  const storage = new MemoryStorage();
  assert.equal(loadLocalePreference(storage), "system");

  saveLocalePreference("en", storage);
  assert.equal(storage.values.get(CONTROLLER_LOCALE_STORAGE_KEY), "en");
  assert.equal(loadLocalePreference(storage), "en");

  storage.values.set(CONTROLLER_LOCALE_STORAGE_KEY, "<script>unexpected</script>");
  assert.equal(loadLocalePreference(storage), "system");

  const blocked: KeyValueStorage = {
    getItem: () => {
      throw new Error("storage disabled");
    },
    setItem: () => {
      throw new Error("storage disabled");
    },
  };
  assert.equal(loadLocalePreference(blocked), "system");
  assert.doesNotThrow(() => saveLocalePreference("zh-CN", blocked));
});

test("a throwing localStorage getter cannot prevent controller initialization", () => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  // Node's test host exposes either no localStorage or a configurable one.
  // Keep this guard so the regression test remains valid in stricter hosts.
  if (previous && !previous.configurable) return;

  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    get() {
      throw new Error("SecurityError: storage access denied");
    },
  });
  try {
    assert.equal(loadLocalePreference(), "system");
    assert.doesNotThrow(() => saveLocalePreference("zh-CN"));
  } finally {
    if (previous) Object.defineProperty(globalThis, "localStorage", previous);
    else Reflect.deleteProperty(globalThis, "localStorage");
  }
});

test("first native launch migrates only a manual legacy preference", () => {
  assert.equal(nativePreferenceMigration(false, "en"), "en");
  assert.equal(nativePreferenceMigration(false, "zh-CN"), "zh-CN");
  assert.equal(nativePreferenceMigration(false, "system"), null);
  assert.equal(nativePreferenceMigration(true, "en"), null);
});

test("dictionaries interpolate values and preserve controller-owned safety copy", () => {
  assert.equal(
    translate("en", "dialog.claimedPackage", { name: "@cordisjs/example" }),
    "Package claimed by link: @cordisjs/example",
  );
  assert.equal(
    translate("zh-CN", "plugin.operationFailed", { operation: "安装", detail: "exit 1" }),
    "安装失败：exit 1",
  );
  assert.match(translate("en", "dialog.deepLinkWarning"), /Rust-validated Cordis slug/);
  assert.equal(translate("en", "window.controllerTitle"), "DSH Desktop — Controller");
  assert.equal(translate("zh-CN", "window.controllerTitle"), "DSH Desktop — 控制器");
  assert.match(translate("en", "diagnostics.modeDescription"), /stderr/);
  assert.match(translate("zh-CN", "diagnostics.modeWarning"), /私密/);
  assert.match(translate("en", "diagnostics.modeDisabledSessionOnly"), /revert/);
  assert.match(formatControllerDate("en", 0), /1970/);
});
