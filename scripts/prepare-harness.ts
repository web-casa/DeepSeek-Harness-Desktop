// Prepare the bundled Harness runtime:
//   pnpm install (frozen lockfile, production) in runtime/  →
//   copy node_modules + attribution into src-tauri/resources/runtime/harness/
//
// The runtime/package.json pins @deepseek-ai/dsh exactly; runtime-manifest.json
// is cross-checked so the bundle and the manifest can never drift.

import { existsSync, readFileSync, rmSync, mkdirSync, cpSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { repoRoot, runtimeDir, harnessDir, readManifest, fail, ok, info } from "./lib/common.ts";

function copyDirectoryPreservingSymlinks(src: string, dest: string): void {
  if (process.platform !== "win32") {
    cpSync(src, dest, { recursive: true, verbatimSymlinks: true });
    return;
  }

  const res = spawnSync("robocopy", [
    src,
    dest,
    "/E",
    "/COPY:DAT",
    "/DCOPY:DAT",
    "/R:1",
    "/W:1",
    "/NFL",
    "/NDL",
    "/NJH",
    "/NJS",
  ], { stdio: "inherit" });
  if (res.status === null || res.status < 0 || res.status > 7) {
    fail(`robocopy failed with exit code ${res.status}`);
  }
}

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

info(`installing @deepseek-ai/dsh@${pinned} (production) into runtime/`);
const storeDir = join(repoRoot, ".pnpm-store");
mkdirSync(storeDir, { recursive: true });
const env: NodeJS.ProcessEnv = {
  ...process.env,
  XDG_CACHE_HOME: join(repoRoot, ".tmp", "xdg-cache"),
  npm_config_cache: join(repoRoot, ".tmp", "npm-cache"),
};
const lockfile = join(repoRoot, "runtime", "pnpm-lock.yaml");
const installArgs = existsSync(lockfile)
  ? ["install", "--prod", "--frozen-lockfile", "--store-dir", storeDir]
  : ["install", "--prod", "--store-dir", storeDir];
const res = spawnSync("pnpm", installArgs, { cwd: join(repoRoot, "runtime"), env, stdio: "inherit" });
if (res.status !== 0) fail("pnpm install failed");

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
copyDirectoryPreservingSymlinks(
  join(repoRoot, "runtime", "node_modules"),
  join(harnessDir, "node_modules"),
);
if (!existsSync(join(harnessDir, "node_modules", "@deepseek-ai", "dsh", "package.json"))) {
  fail("staged @deepseek-ai/dsh package.json missing");
}
cpSync(runtimePkgPath, join(harnessDir, "package.json"));

// Attribution: LICENSE from the dsh package; per-dependency notices come in P3.
const dshDir = join(harnessDir, "node_modules", "@deepseek-ai", "dsh");
for (const file of ["LICENSE", "THIRD_PARTY_NOTICES.md", "README.zh.md", "README.md"]) {
  const src = join(dshDir, file);
  if (existsSync(src)) cpSync(src, join(harnessDir, file));
}

// A copy of the manifest travels with the bundle so About/diagnostics can
// report exact versions without network access.
writeFileSync(
  join(harnessDir, "runtime-manifest.json"),
  JSON.stringify(manifest, null, 2) + "\n",
);

ok(`harness runtime staged at ${harnessDir}`);
info(`@deepseek-ai/dsh ${installedVersion} · desktop ${manifest.desktopVersion}`);
