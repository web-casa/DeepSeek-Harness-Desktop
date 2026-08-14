// Prepare the bundled Harness runtime:
//   npm ci (production, flat layout) in runtime/  →
//   materialize node_modules + attribution into src-tauri/resources/runtime/harness/
//
// npm's flat layout is what keeps paths short (Windows MAX_PATH) and lets
// Node resolve @deepseek-ai/* from the real top-level tree; the materializer
// then removes every remaining symlink (.bin shims, nested links).
//
// The runtime/package.json pins @deepseek-ai/dsh exactly; runtime-manifest.json
// is cross-checked so the bundle and the manifest can never drift.

import { existsSync, readFileSync, rmSync, mkdirSync, writeFileSync, copyFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { repoRoot, runtimeDir, harnessDir, readManifest, fail, ok, info } from "./lib/common.ts";
import { materialize } from "./lib/materialize.ts";

const manifest = readManifest();
const runtimePkgPath = join(repoRoot, "runtime", "package.json");
const runtimePkg = JSON.parse(readFileSync(runtimePkgPath, "utf8")) as {
  dependencies?: Record<string, string>;
};
const pinned = runtimePkg.dependencies?.["@deepseek-ai/dsh"];

// Cross-check: the manifest is the single source of truth.
if (pinned !== manifest.harnessVersion) {
  fail(
    `version drift: runtime/package.json pins @deepseek-ai/dsh@${pinned} but ` +
      `runtime-manifest.json says ${manifest.harnessVersion}`,
  );
}

info(`installing @deepseek-ai/dsh@${pinned} (production, npm flat) into runtime/`);
const env: NodeJS.ProcessEnv = {
  ...process.env,
  XDG_CACHE_HOME: join(repoRoot, ".tmp", "xdg-cache"),
  npm_config_cache: join(repoRoot, ".tmp", "npm-cache"),
};
// npm ci: clean, reproducible install from the committed package-lock.json.
// On Windows the npm shim is a .cmd file; spawnSync resolves it via a shell.
const res = spawnSync("npm", ["ci", "--omit=dev", "--cache", env.npm_config_cache], {
  cwd: join(repoRoot, "runtime"),
  env,
  stdio: "inherit",
  shell: process.platform === "win32",
});
if (res.status !== 0) {
  const detail = res.error ? ` (${res.error.message})` : ` (exit ${res.status})`;
  fail(`npm ci failed${detail}`);
}

// Verify the installed version matches the manifest.
const installedPkgPath = join(
  repoRoot,
  "runtime",
  "node_modules",
  "@deepseek-ai",
  "dsh",
  "package.json",
);
if (!existsSync(installedPkgPath)) fail("installed @deepseek-ai/dsh package.json missing");
const installedVersion = (
  JSON.parse(readFileSync(installedPkgPath, "utf8")) as { version?: string }
).version;
if (installedVersion !== manifest.harnessVersion) {
  fail(`installed @deepseek-ai/dsh@${installedVersion} != manifest ${manifest.harnessVersion}`);
}

// Stage into the bundle resources dir.
rmSync(harnessDir, { recursive: true, force: true });
mkdirSync(harnessDir, { recursive: true });
materialize(
  join(repoRoot, "runtime", "node_modules"),
  join(harnessDir, "node_modules"),
);
if (!existsSync(join(harnessDir, "node_modules", "@deepseek-ai", "dsh", "package.json"))) {
  fail("staged @deepseek-ai/dsh package.json missing");
}
copyFileSync(runtimePkgPath, join(harnessDir, "package.json"));

// Attribution: LICENSE from the dsh package; per-dependency notices come in P3.
const dshDir = join(harnessDir, "node_modules", "@deepseek-ai", "dsh");
for (const file of ["LICENSE", "THIRD_PARTY_NOTICES.md", "README.zh.md", "README.md"]) {
  const src = join(dshDir, file);
  if (existsSync(src)) materialize(src, join(harnessDir, file));
}

// A copy of the manifest travels with the bundle so About/diagnostics can
// report exact versions without network access.
writeFileSync(
  join(harnessDir, "runtime-manifest.json"),
  JSON.stringify(manifest, null, 2) + "\n",
);

ok(`harness runtime staged at ${harnessDir}`);
info(`@deepseek-ai/dsh ${installedVersion} · desktop ${manifest.desktopVersion}`);
