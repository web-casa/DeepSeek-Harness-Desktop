// Preset discovery e2e: proves the shell-side importer's output lands in the
// exact location upstream's own discovery function reads — without needing
// to run the Harness UI. Drives the REAL `discoverPresets` export from
// @deepseek-ai/dsh-agent-presets with the bundled Node.
//
// Runs where the runtime is staged (smoke job): it simulates the importer's
// on-disk result (<dsh_home>/.agent-presets/<id>/{preset.yml,agent.cordis.yml})
// and asserts upstream discovers it with trust "user" and no `broken` marker.

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { harnessDir, tmpDir, fail, ok, info } from "./lib/common.ts";

const dshHome = join(tmpDir, "preset-e2e-home");
const id = "e2e-demo";

async function main(): Promise<void> {
  rmSync(dshHome, { recursive: true, force: true });
  mkdirSync(join(dshHome, ".agent-presets", id), { recursive: true });
  writeFileSync(
    join(dshHome, ".agent-presets", id, "preset.yml"),
    "name: E2E demo\ndescription: discovery proof\norder: 9\n",
  );
  // A minimal VALID composition: a top-level list of plugin rows (the
  // shape upstream's compositionProblem validates).
  writeFileSync(
    join(dshHome, ".agent-presets", id, "agent.cordis.yml"),
    "- id: persona\n  name: '@deepseek-ai/dsh-persona'\n  config:\n    text: |-\n      E2E discovery proof.\n",
  );

  const systemRoot = join(
    harnessDir,
    "node_modules",
    "@deepseek-ai",
    "dsh",
    "config",
    "agent-presets",
  );
  if (!existsSync(systemRoot)) fail("shipped agent-presets dir missing");

  // Drive the REAL upstream discovery with the bundled Node. The module path
  // is the same one the Harness UI resolves through its import map.
  const script = `
import { discoverPresets } from "./node_modules/@deepseek-ai/dsh-agent-presets/lib/index.js";
const rows = await discoverPresets([
  { path: ${JSON.stringify(join(dshHome, ".agent-presets"))}, trust: "user" },
  { path: ${JSON.stringify(systemRoot)}, trust: "system" },
]);
const hit = rows.find((r) => r.id === ${JSON.stringify(id)});
console.log(JSON.stringify({ found: Boolean(hit), trust: hit?.trust ?? null, broken: hit?.broken ?? null, total: rows.length }));
`;
  const nodePath = join(harnessDir, "..", "node");
  const child = spawn(nodePath, ["--input-type=module", "-e", script], {
    cwd: harnessDir,
    stdio: ["ignore", "pipe", "inherit"],
  });
  let out = "";
  child.stdout.on("data", (c: Buffer) => {
    out += c.toString("utf8");
  });
  const code = await new Promise<number>((resolve) => child.on("exit", resolve));
  rmSync(dshHome, { recursive: true, force: true });
  if (code !== 0) fail(`discovery driver exited ${code}`);
  const result = JSON.parse(out.trim().split("\n").pop() ?? "{}") as {
    found?: boolean;
    trust?: string | null;
    broken?: string | null;
    total?: number;
  };
  if (!result.found) fail(`upstream did not discover ${id}: ${JSON.stringify(result)}`);
  if (result.trust !== "user") fail(`preset must be trusted "user", got ${result.trust}`);
  if (result.broken) fail(`preset marked broken: ${result.broken}`);
  ok(`upstream discovers ${id} (trust=user, not broken, ${result.total} presets total)`);
  console.log("\n  PASS — preset discovery e2e complete");
}

main().catch((e: Error) => {
  rmSync(dshHome, { recursive: true, force: true });
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
