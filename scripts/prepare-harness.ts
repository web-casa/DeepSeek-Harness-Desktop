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

import {
  existsSync,
  readFileSync,
  rmSync,
  writeFileSync,
  copyFileSync,
  renameSync,
  mkdirSync,
  readdirSync,
  type Dirent,
} from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { repoRoot, runtimeDir, harnessDir, readManifest, fail, ok, info } from "./lib/common.ts";
import { materialize } from "./lib/materialize.ts";

// Collect every package's license file into licenses/<rel-path> so the
// installer carries third-party attribution for the whole dependency tree.
// One recursive walk covers ALL package locations: top-level, scoped
// (@scope/<name>), and nested node_modules (un-hoisted version conflicts).
// A directory is a package iff it contains package.json — the tree walk
// never assumes a specific layout, so npm layout changes cannot silently
// drop attribution again.
function collectLicenses(nodeModules: string, outDir: string): void {
  let collected = 0;
  const candidates = ["LICENSE", "LICENSE.md", "LICENSE.txt", "LICENCE", "COPYING"];
  const walk = (dir: string, rel: string): void => {
    let entries: Dirent[];
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return; // unreadable subtree (e.g. dangling entry) — not a package
    }
    if (entries.some((e) => e.isFile() && e.name === "package.json")) {
      for (const file of candidates) {
        const src = join(dir, file);
        if (existsSync(src)) {
          const destDir = join(outDir, rel);
          mkdirSync(destDir, { recursive: true });
          copyFileSync(src, join(destDir, file));
          collected += 1;
          break;
        }
      }
    }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      walk(join(dir, entry.name), rel === "" ? entry.name : join(rel, entry.name));
    }
  };
  walk(nodeModules, "");
  info(`collected ${collected} third-party license files into licenses/`);
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

info(`installing @deepseek-ai/dsh@${pinned} (production, npm flat) into runtime/`);
const env: NodeJS.ProcessEnv = {
  ...process.env,
  XDG_CACHE_HOME: join(repoRoot, ".tmp", "xdg-cache"),
  npm_config_cache: join(repoRoot, ".tmp", "npm-cache"),
};

// The runtime/.npmrc allowlist (allow-scripts/strict-allow-scripts) requires
// npm >= 11.17. Older npm silently IGNORES unknown config keys — the allowlist
// would fail open and every dependency script would run unreviewed.
const npmVersionRes = spawnSync("npm", ["--version"], { encoding: "utf8", shell: process.platform === "win32" });
const npmVersion = (npmVersionRes.stdout ?? "").trim();
const [vMajor = 0, vMinor = 0] = npmVersion.split(".").map((p) => Number.parseInt(p, 10) || 0);
if (npmVersionRes.status !== 0 || vMajor < 11 || (vMajor === 11 && vMinor < 17)) {
  fail(
    `npm ${npmVersion || "not found"} is too old: strict-allow-scripts requires npm >= 11.17. ` +
      "Upgrade Node (npm ships with it) before preparing the runtime.",
  );
}

// npm ci: clean, reproducible install from the committed package-lock.json.
// On Windows the npm shim is a .cmd file; spawnSync resolves it via a shell.
// The cache path travels via npm_config_cache — no --cache CLI flag needed
// (and no argument quoting issues with spaces in the repo path).
const res = spawnSync("npm", ["ci", "--omit=dev"], {
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

// Stage atomically: materialize into a sibling temp dir, validate, then swap.
// A failed or concurrent build must never observe a half-written runtime.
const staging = `${harnessDir}.staging-${process.pid}`;
rmSync(staging, { recursive: true, force: true });
try {
  materialize(
    join(repoRoot, "runtime", "node_modules"),
    join(staging, "node_modules"),
  );
  if (!existsSync(join(staging, "node_modules", "@deepseek-ai", "dsh", "package.json"))) {
    throw new Error("staged @deepseek-ai/dsh package.json missing");
  }
  copyFileSync(runtimePkgPath, join(staging, "package.json"));

  // Attribution: dsh's own license/readme at the top level, plus a per-package
  // license tree so the installer carries third-party notices.
  const dshDir = join(staging, "node_modules", "@deepseek-ai", "dsh");
  for (const file of ["LICENSE", "THIRD_PARTY_NOTICES.md", "README.zh.md", "README.md"]) {
    const src = join(dshDir, file);
    if (existsSync(src)) materialize(src, join(staging, file));
  }
  collectLicenses(
    join(staging, "node_modules"),
    join(staging, "licenses"),
  );

  // A copy of the manifest travels with the bundle so About/diagnostics can
  // report exact versions without network access.
  writeFileSync(
    join(staging, "runtime-manifest.json"),
    JSON.stringify(manifest, null, 2) + "\n",
  );

  rmSync(harnessDir, { recursive: true, force: true });
  renameSync(staging, harnessDir);
} catch (e) {
  rmSync(staging, { recursive: true, force: true });
  throw e;
}

ok(`harness runtime staged at ${harnessDir}`);
info(`@deepseek-ai/dsh ${installedVersion} · desktop ${manifest.desktopVersion}`);
