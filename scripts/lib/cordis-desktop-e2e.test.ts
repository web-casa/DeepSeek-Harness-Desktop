import assert from "node:assert/strict";
import { test } from "node:test";
import {
  DEFAULT_PRODUCTION_E2E_SLUG,
  PRODUCTION_CORDIS_API,
  marketInstallCandidateFromValue,
  parseProductionE2eConfig,
} from "./cordis-desktop-e2e.ts";

const app = "/tmp/deepseek-harness-desktop";

test("production Desktop lifecycle e2e requires an explicit opt-in and exact Cordis origin", () => {
  assert.throws(
    () => parseProductionE2eConfig({ CORDIS_DESKTOP_E2E_APP: app }, "linux"),
    /explicitly/,
  );
  assert.throws(
    () =>
      parseProductionE2eConfig(
        {
          CORDIS_DESKTOP_PRODUCTION_E2E: "1",
          CORDIS_DESKTOP_E2E_APP: app,
          CORDIS_RUN_API: "http://127.0.0.1:3900/api/v1",
        },
        "linux",
      ),
    /must be exactly/,
  );
  assert.throws(
    () =>
      parseProductionE2eConfig(
        { CORDIS_DESKTOP_PRODUCTION_E2E: "1", CORDIS_DESKTOP_E2E_APP: "relative-app" },
        "linux",
      ),
    /absolute/,
  );
  assert.throws(
    () =>
      parseProductionE2eConfig(
        { CORDIS_DESKTOP_PRODUCTION_E2E: "1", CORDIS_DESKTOP_E2E_APP: app },
        "win32",
      ),
    /Linux only/,
  );
});

test("production Desktop lifecycle e2e retains the reviewed defaults", () => {
  assert.deepEqual(
    parseProductionE2eConfig(
      {
        CORDIS_DESKTOP_PRODUCTION_E2E: "1",
        CORDIS_DESKTOP_E2E_APP: app,
        CORDIS_RUN_API: PRODUCTION_CORDIS_API,
      },
      "linux",
    ),
    {
      application: app,
      slug: DEFAULT_PRODUCTION_E2E_SLUG,
      tauriDriver: "tauri-driver",
      nativeDriver: "WebKitWebDriver",
      useXvfb: true,
    },
  );
});

test("the production lifecycle runner accepts only a complete sha512 market candidate", () => {
  const candidate = {
    slug: DEFAULT_PRODUCTION_E2E_SLUG,
    entryRevision: "revision-1",
    packageName: "dsh-plugin-pkgseek",
    version: "0.1.1",
    integrity: "sha512-example",
    registry: "https://registry.npmjs.org",
    tarball: "https://registry.npmjs.org/dsh-plugin-pkgseek/-/dsh-plugin-pkgseek-0.1.1.tgz",
  };
  assert.deepEqual(
    marketInstallCandidateFromValue(candidate, DEFAULT_PRODUCTION_E2E_SLUG),
    candidate,
  );
  assert.throws(
    () =>
      marketInstallCandidateFromValue(
        { ...candidate, integrity: "sha256-example" },
        candidate.slug,
      ),
    /sha512/,
  );
  assert.throws(
    () => marketInstallCandidateFromValue({ ...candidate, slug: "other" }, candidate.slug),
    /expected/,
  );
});
