import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { repoRoot } from "./common.ts";

const launchFiles = [
  "src-tauri/src/harness/mod.rs",
  "scripts/verify-runtime.ts",
  "scripts/load-soak.ts",
] as const;

const expectedArgs =
  '["web","--no-open","--host","127.0.0.1","--port","0"]';

test("Desktop and both real-runtime probes suppress upstream browser opening", () => {
  for (const relative of launchFiles) {
    const source = readFileSync(join(repoRoot, relative), "utf8").replaceAll(/\s/g, "");
    assert.ok(
      source.includes(expectedArgs),
      `${relative} must launch dsh web with the reviewed ${expectedArgs} contract`,
    );
  }
});
