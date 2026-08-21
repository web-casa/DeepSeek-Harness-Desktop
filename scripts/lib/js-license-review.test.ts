import assert from "node:assert/strict";
import { test } from "node:test";
import { reviewJavaScriptLicenses } from "./js-license-review.ts";

test("accepts allowed pnpm SPDX expressions", () => {
  const result = reviewJavaScriptLicenses(
    {
      "MIT OR Apache-2.0": [
        {
          name: "safe-package",
          versions: ["1.2.3"],
          license: "MIT OR Apache-2.0",
        },
      ],
    },
    "pnpm",
  );
  assert.deepEqual(result, { checked: 1, violations: [] });
});

test("rejects denied pnpm licenses inside compound expressions", () => {
  const result = reviewJavaScriptLicenses(
    {
      "MIT AND GPL-3.0-only": [
        { name: "unsafe-package", versions: ["4.5.6"] },
      ],
    },
    "pnpm",
  );
  assert.deepEqual(result.violations, [
    "unsafe-package@4.5.6: MIT AND GPL-3.0-only (denies GPL-3.0-only)",
  ]);
});

test("reviews installed npm dependencies but skips the private root", () => {
  const result = reviewJavaScriptLicenses(
    [
      { name: "workspace", version: "1.0.0", location: "" },
      {
        name: "legacy-package",
        version: "2.0.0",
        location: "node_modules/legacy-package",
        license: { type: "LGPL-2.1-only" },
      },
      {
        name: "safe-package",
        version: "3.0.0",
        location: "node_modules/safe-package",
        license: "ISC",
      },
    ],
    "npm-query",
  );
  assert.equal(result.checked, 2);
  assert.deepEqual(result.violations, [
    "legacy-package@2.0.0: LGPL-2.1-only (denies LGPL-2.1-only)",
  ]);
});

test("fails closed on malformed reports", () => {
  assert.throws(() => reviewJavaScriptLicenses([], "pnpm"), /must be an object/);
  assert.throws(
    () => reviewJavaScriptLicenses({ MIT: {} }, "pnpm"),
    /must be an array/,
  );
  assert.throws(() => reviewJavaScriptLicenses({}, "npm-query"), /must be an array/);
});
