import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { repoRoot } from "./common.ts";

const source = readFileSync(join(repoRoot, "src-tauri/src/profile_fallback.rs"), "utf8");
const harnessSource = readFileSync(join(repoRoot, "src-tauri/src/harness/mod.rs"), "utf8");

test("Desktop profile recovery calls the reviewed upstream public fallback healer", () => {
  assert.match(source, /import \{ healProfilesModuleFallback \} from "@deepseek-ai\/dsh-app-boot"/);
  assert.match(source, /healProfilesModuleFallback\(process\.argv\[1\], process\.argv\[2\]\)/);
  assert.match(source, /script: "--input-type=module"/);
  assert.match(source, /"-e"\.to_string\(\)/);
});

test("Desktop profile recovery never mutates profile-local user node_modules", () => {
  const quarantineStart = source.indexOf("fn quarantine_core_scope");
  const quarantineEnd = source.indexOf("fn repair_spawn_spec");
  assert.ok(quarantineStart >= 0 && quarantineEnd > quarantineStart, "quarantine implementation must exist");
  const quarantine = source.slice(quarantineStart, quarantineEnd);
  assert.match(quarantine, /join\("profiles"\)\.join\("node_modules"\)/);
  assert.doesNotMatch(quarantine, /join\("profiles"\)\.join\("web"\)/);
  assert.match(quarantine, /fs::rename\(&scope, &backup\)/);
});

test("a Desktop-owned fallback repair failure never unlocks plugin recovery", () => {
  const repairStart = harnessSource.indexOf("match crate::profile_fallback::repair_if_needed");
  const launchStart = harnessSource.indexOf("if let Err(error) = launch_sidecar", repairStart);
  assert.ok(repairStart >= 0 && launchStart > repairStart, "fallback launch boundary must exist");
  const repairBoundary = harnessSource.slice(repairStart, launchStart);
  assert.match(repairBoundary, /profile_fallback_repair_failed/);
  assert.match(repairBoundary, /terminal_startup_failure = false/);
  assert.doesNotMatch(repairBoundary, /terminal_startup_failure = true/);
});
