// Plugin installation e2e: drives both REAL chains the desktop shell spawns
// against a fresh, isolated DSH_HOME — bundled node → dsh lib/bin.js
// `plugin --profile web add` → shim-resolved bundled pnpm → upstream
// initProfile/reconcilePlugins, plus Desktop's direct bundled-pnpm market-add
// and removal paths.
//
// What this proves:
//   - the shim + PATH setup lets upstream's `spawnSync("pnpm", …)` find the
//     BUNDLED pnpm (never a system pnpm);
//   - initProfile materializes profiles/web/{package.json,cordis.patch.yml,
//     pnpm-workspace.yaml} exactly as upstream writes them;
//   - `add is-odd` installs into profile dependencies and the installed
//     version is readable from node_modules/<pkg>/package.json (the same
//     source `list_plugins` reads);
//   - reconcilePlugins runs (is-odd declares no dsh.bundle → warning +
//     in-box bundles list untouched);
//   - Desktop's direct, scripts-disabled pnpm removal drops the dependency
//     without invoking upstream's global bundle reconciliation;
//   - the reported dsh-plugin-pkgseek@0.1.1 active-bundle uninstall path
//     removes both its dependency and dsh.profile.bundles entry;
//   - removing another package cannot reactivate a pending bundle.
//
// Store isolation (plan S5): `pnpm_config_store_dir` pins the pnpm content
// store inside the temp home, so nothing touches the user's real store.
// (pnpm reads env config under the `pnpm_config_` prefix — verified against
// the bundled pnpm 11.21.0; `npm_config_*` is NOT honored for this key.)
// NOTE: requires network access to registry.npmjs.org (CI runners have it).
//
// The Rust side of the shim text/exec is covered by plugins.rs tests. Both
// plugin smoke programs share scripts/lib/plugin-shim.ts, whose quoting and
// multi-segment PATH contract has its own Node test; this avoids the two e2e
// paths silently carrying different, stale shim copies.

import { spawn } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import {
  runtimeDir as defaultRuntimeDir,
  tmpDir,
  fail,
  ok,
  info,
} from "./lib/common.ts";
import {
  pluginMarketAddArgs,
  pluginPath,
  pluginRemoveArgs,
  writePluginShims,
} from "./lib/plugin-shim.ts";

const verbose = process.argv.includes("--verbose");
const runtimeDirArg = process.argv.indexOf("--runtime-dir");
if (runtimeDirArg >= 0 && !process.argv[runtimeDirArg + 1]) {
  fail("--runtime-dir requires a path");
}
const rtDir = runtimeDirArg >= 0 ? resolve(process.argv[runtimeDirArg + 1]) : defaultRuntimeDir;
const harness = join(rtDir, "harness");
const node = join(rtDir, `node${process.platform === "win32" ? ".exe" : ""}`);
const dshBin = join(harness, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
const pnpmCjs = join(harness, "node_modules", "pnpm", "bin", "pnpm.cjs");

function requireFile(path: string, label: string): void {
  if (!existsSync(path)) {
    fail(`${label} missing at ${path} — run scripts/download-node.ts / prepare-harness.ts first`);
  }
}

requireFile(node, "bundled node");
requireFile(dshBin, "dsh entry (lib/bin.js)");
requireFile(pnpmCjs, "bundled pnpm (bin/pnpm.cjs)");

const home = join(tmpDir, `plugin-e2e-home-${Date.now()}`);
const profileDir = join(home, "profiles", "web");
const toolsDir = join(home, ".desktop-tools");
const storeDir = join(home, "pnpm-store");
const configDir = join(toolsDir, "pnpm-config");

function writeShims(): void {
  writePluginShims(toolsDir, node, pnpmCjs);
  mkdirSync(configDir, { recursive: true });
  writeFileSync(join(configDir, ".npmrc"), "");
}

function pluginEnv(): NodeJS.ProcessEnv {
  return {
    ...process.env,
    DSH_HOME: home,
    DSH_TELEMETRY_DISABLED: "1",
    PATH: pluginPath(toolsDir, process.env.PATH),
    pnpm_config_store_dir: storeDir,
  };
}

function directPnpmEnv(): NodeJS.ProcessEnv {
  return {
    ...pluginEnv(),
    XDG_CONFIG_HOME: configDir,
    NPM_CONFIG_USERCONFIG: join(configDir, ".npmrc"),
  };
}

function runPlugin(args: string[], label: string): Promise<string> {
  return new Promise((resolvePromise, reject) => {
    // detached on unix: the plugin tree gets its own process group so the
    // timeout path can kill node → dsh → pnpm as a unit (the shell's
    // PlatformChild gives the same guarantee in production).
    const child = spawn(node, [dshBin, "plugin", "--profile", "web", ...args], {
      cwd: harness,
      env: pluginEnv(),
      stdio: ["ignore", "pipe", "pipe"],
      detached: process.platform !== "win32",
    });
    let output = "";
    const collect = (c: Buffer): void => {
      output += c.toString("utf8");
      if (verbose) process.stdout.write(c);
    };
    child.stdout.on("data", collect);
    child.stderr.on("data", collect);
    const timer = setTimeout(() => {
      if (process.platform === "win32") {
        spawn("taskkill", ["/pid", String(child.pid), "/T", "/F"]);
      } else {
        try {
          process.kill(-(child.pid as number), "SIGKILL");
        } catch {
          /* already exited */
        }
      }
      reject(new Error(`timeout running ${label}:\n${output.slice(-4000)}`));
    }, 300_000);
    child.on("error", (e) => {
      clearTimeout(timer);
      reject(new Error(`cannot spawn ${label}: ${e.message}`));
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      if (code !== 0) reject(new Error(`${label} exited ${code}:\n${output.slice(-4000)}`));
      else resolvePromise(output);
    });
  });
}

function boundStoreBase(): string {
  const modulesState = JSON.parse(
    readFileSync(join(profileDir, "node_modules", ".modules.yaml"), "utf8"),
  ) as { storeDir?: unknown };
  if (typeof modulesState.storeDir !== "string") {
    fail("pnpm .modules.yaml did not record a string storeDir");
  }
  return dirname(modulesState.storeDir);
}

function runDirectPnpm(args: string[], label: string): Promise<string> {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(
      node,
      [pnpmCjs, ...args],
      {
        cwd: profileDir,
        env: directPnpmEnv(),
        stdio: ["ignore", "pipe", "pipe"],
        detached: process.platform !== "win32",
      },
    );
    let output = "";
    const collect = (chunk: Buffer): void => {
      output += chunk.toString("utf8");
      if (verbose) process.stdout.write(chunk);
    };
    child.stdout.on("data", collect);
    child.stderr.on("data", collect);
    const timer = setTimeout(() => {
      if (process.platform === "win32") {
        spawn("taskkill", ["/pid", String(child.pid), "/T", "/F"]);
      } else {
        try {
          process.kill(-(child.pid as number), "SIGKILL");
        } catch {
          /* already exited */
        }
      }
      reject(new Error(`timeout running ${label}:\n${output.slice(-4000)}`));
    }, 300_000);
    child.on("error", (error) => {
      clearTimeout(timer);
      reject(new Error(`cannot spawn ${label}: ${error.message}`));
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      if (code !== 0) reject(new Error(`${label} exited ${code}:\n${output.slice(-4000)}`));
      else resolvePromise(output);
    });
  });
}

function runPnpmRemove(name: string, label: string): Promise<string> {
  return runDirectPnpm(pluginRemoveArgs(name, configDir, boundStoreBase()), label);
}

function runPnpmMarketAdd(tarball: string, label: string): Promise<string> {
  return runDirectPnpm(
    pluginMarketAddArgs(
      tarball,
      "https://registry.npmjs.org",
      configDir,
      boundStoreBase(),
    ),
    label,
  );
}

interface ProfileManifest {
  dependencies?: Record<string, string>;
  dsh?: { profile?: { bundles?: string[] } };
}

function readProfileManifest(): ProfileManifest {
  return JSON.parse(readFileSync(join(profileDir, "package.json"), "utf8")) as ProfileManifest;
}

function exactLockfileIntegrity(packageName: string, version: string): string | null {
  const expected = `${packageName}@${version}`;
  const lines = readFileSync(join(profileDir, "pnpm-lock.yaml"), "utf8").split(/\r?\n/);
  let inPackages = false;
  let inTarget = false;
  for (const line of lines) {
    if (line === "packages:") {
      inPackages = true;
      continue;
    }
    if (!inPackages) continue;
    if (line === "snapshots:") break;
    const keyMatch = /^  (.+):$/.exec(line);
    if (keyMatch) {
      const raw = keyMatch[1];
      const key =
        raw.length >= 2 &&
        ((raw.startsWith("'") && raw.endsWith("'")) ||
          (raw.startsWith('"') && raw.endsWith('"')))
          ? raw.slice(1, -1)
          : raw;
      inTarget = key === expected || key.startsWith(`${expected}(`);
      continue;
    }
    if (!inTarget) continue;
    const integrity = /(?:^|[,{]\s*)integrity:\s*([^,}\s]+)/.exec(line)?.[1];
    if (integrity) return integrity.replace(/^['"]|['"]$/g, "");
  }
  return null;
}

function preDisable(name: string): void {
  const manifest = readProfileManifest();
  const bundles = manifest.dsh?.profile?.bundles;
  if (!Array.isArray(bundles)) fail("profile bundle list is missing before pre-disable");
  manifest.dsh = {
    ...manifest.dsh,
    profile: {
      ...manifest.dsh?.profile,
      bundles: bundles.filter((bundle) => bundle !== name),
    },
  };
  writeFileSync(join(profileDir, "package.json"), JSON.stringify(manifest, null, 2) + "\n");
}

async function main(): Promise<void> {
  rmSync(home, { recursive: true, force: true });
  mkdirSync(home, { recursive: true });
  writeShims();
  info(`isolated DSH_HOME ${home} · store ${storeDir}`);

  // --- add -------------------------------------------------------------
  const addOut = await runPlugin(["add", "is-odd"], "dsh plugin add is-odd");
  ok("dsh plugin add is-odd exited 0");
  if (!existsSync(join(profileDir, "package.json"))) fail("initProfile did not write profiles/web/package.json");
  ok("upstream initProfile wrote profiles/web/package.json");
  if (!existsSync(join(profileDir, "cordis.patch.yml"))) fail("initProfile did not write cordis.patch.yml");
  if (!existsSync(join(profileDir, "pnpm-workspace.yaml"))) fail("initProfile did not write pnpm-workspace.yaml");
  const workspace = readFileSync(join(profileDir, "pnpm-workspace.yaml"), "utf8");
  if (!workspace.includes("nodeLinker: hoisted")) fail(`pnpm-workspace.yaml missing nodeLinker: hoisted:\n${workspace}`);
  ok("initProfile materialized cordis.patch.yml + pnpm-workspace.yaml (hoisted)");

  const manifest = readProfileManifest();
  const deps = manifest.dependencies ?? {};
  if (typeof deps["is-odd"] !== "string") {
    fail(`is-odd missing from profile dependencies: ${JSON.stringify(manifest.dependencies)}`);
  }
  ok(`profile dependencies include is-odd (${deps["is-odd"]})`);

  const bundles = manifest.dsh?.profile?.bundles ?? [];
  if (bundles.includes("is-odd")) fail(`reconcilePlugins must NOT add a bundle-less package to the layer list: ${JSON.stringify(bundles)}`);
  if (bundles.length !== 2) fail(`in-box bundles changed unexpectedly: ${JSON.stringify(bundles)}`);
  if (!addOut.includes("declares no dsh.bundle")) fail("reconcilePlugins did not warn about the bundle-less dependency");
  ok("reconcilePlugins ran: bundle-less warning emitted, in-box layer list untouched");

  const installedVersion = JSON.parse(
    readFileSync(join(profileDir, "node_modules", "is-odd", "package.json"), "utf8"),
  ) as { version?: string };
  if (typeof installedVersion.version !== "string" || !installedVersion.version.startsWith("3.")) {
    fail(`unexpected installed is-odd version: ${JSON.stringify(installedVersion.version)}`);
  }
  ok(`installed version readable from node_modules/is-odd/package.json (v${installedVersion.version})`);

  if (!existsSync(storeDir)) fail("pnpm_config_store_dir was not respected: store dir not created");
  ok("pnpm store isolated via pnpm_config_store_dir");

  const pkgseekTarball =
    "https://registry.npmjs.org/dsh-plugin-pkgseek/-/dsh-plugin-pkgseek-0.1.1.tgz";
  const pkgseekIntegrity =
    "sha512-CdpaMsTQBGgpRfgDLT1+FV0JWlOTJUGed3JEzYYz5IVMwjjncWQ3qj/lgd6NIvoV9YMBHF9p+7ZAyt09f8Q5uA==";
  const pnpmfileMarker = join(home, "pnpmfile-was-loaded");
  writeFileSync(
    join(profileDir, ".pnpmfile.cjs"),
    `require("node:fs").writeFileSync(${JSON.stringify(pnpmfileMarker)}, "unsafe"); module.exports = { hooks: {} };\n`,
  );
  await runPnpmMarketAdd(pkgseekTarball, "direct market pnpm add dsh-plugin-pkgseek@0.1.1");
  if (existsSync(pnpmfileMarker)) {
    fail("direct market install executed the profile-local pnpmfile");
  }
  rmSync(join(profileDir, ".pnpmfile.cjs"), { force: true });
  const pendingPkgseek = readProfileManifest();
  if (pendingPkgseek.dependencies?.["dsh-plugin-pkgseek"] !== pkgseekTarball) {
    fail("direct market install did not retain the exact reviewed tarball source");
  }
  if (pendingPkgseek.dsh?.profile?.bundles?.includes("dsh-plugin-pkgseek")) {
    fail("direct market install activated dsh-plugin-pkgseek before confirmation");
  }
  const installedIntegrity = exactLockfileIntegrity("dsh-plugin-pkgseek", "0.1.1");
  if (installedIntegrity !== pkgseekIntegrity) {
    fail(`direct market lockfile integrity mismatch: ${installedIntegrity}`);
  }
  ok("direct market pnpm install preserved exact source/integrity and remained inactive");

  // Regression for the real package reported by Desktop users. Unlike
  // is-odd it declares dsh.bundle.
  await runPlugin(
    ["add", "dsh-plugin-pkgseek@0.1.1"],
    "dsh plugin add dsh-plugin-pkgseek@0.1.1",
  );
  const activePkgseek = readProfileManifest();
  if (typeof activePkgseek.dependencies?.["dsh-plugin-pkgseek"] !== "string") {
    fail("dsh-plugin-pkgseek missing from dependencies after install");
  }
  if (!activePkgseek.dsh?.profile?.bundles?.includes("dsh-plugin-pkgseek")) {
    fail("dsh-plugin-pkgseek missing from active bundles after install");
  }
  ok("dsh-plugin-pkgseek@0.1.1 installed and activated by upstream reconcile");

  // Model Desktop's exact pre-disable. Keeping pkgseek installed but absent
  // from bundles is the same safety boundary as a market package awaiting
  // explicit Activate.
  preDisable("dsh-plugin-pkgseek");
  if (readProfileManifest().dsh?.profile?.bundles?.includes("dsh-plugin-pkgseek")) {
    fail("pre-disable did not remove dsh-plugin-pkgseek from active bundles");
  }

  // Production intentionally calls pnpm directly for removal. Upstream
  // `dsh plugin remove is-odd` would globally reconcile here and reactivate
  // the still-installed pkgseek bundle as an unrelated side effect.
  writeFileSync(
    join(profileDir, ".pnpmfile.cjs"),
    `require("node:fs").writeFileSync(${JSON.stringify(pnpmfileMarker)}, "unsafe"); module.exports = { hooks: {} };\n`,
  );
  await runPnpmRemove("is-odd", "direct pnpm remove is-odd");
  if (existsSync(pnpmfileMarker)) {
    fail("direct plugin removal executed the profile-local pnpmfile");
  }
  rmSync(join(profileDir, ".pnpmfile.cjs"), { force: true });
  const after = readProfileManifest();
  if (typeof after.dependencies?.["is-odd"] === "string") {
    fail(`is-odd still in dependencies after remove: ${JSON.stringify(after.dependencies)}`);
  }
  if (existsSync(join(profileDir, "node_modules", "is-odd"))) {
    fail("node_modules/is-odd still present after remove");
  }
  if (after.dsh?.profile?.bundles?.includes("dsh-plugin-pkgseek")) {
    fail("removing is-odd reactivated the unrelated pre-disabled pkgseek bundle");
  }
  ok("direct removal dropped is-odd without activating an unrelated pending bundle");

  await runPnpmRemove("dsh-plugin-pkgseek", "direct pnpm remove dsh-plugin-pkgseek");
  const withoutPkgseek = readProfileManifest();
  if (typeof withoutPkgseek.dependencies?.["dsh-plugin-pkgseek"] === "string") {
    fail("dsh-plugin-pkgseek still present in dependencies after remove");
  }
  if (withoutPkgseek.dsh?.profile?.bundles?.includes("dsh-plugin-pkgseek")) {
    fail("dsh-plugin-pkgseek still present in active bundles after remove");
  }
  if (existsSync(join(profileDir, "node_modules", "dsh-plugin-pkgseek"))) {
    fail("node_modules/dsh-plugin-pkgseek still present after remove");
  }
  ok("dsh-plugin-pkgseek uninstall removed dependency, active bundle, and installed files");

  rmSync(home, { recursive: true, force: true });
  console.log("\n  PASS — plugin install/remove e2e complete");
}

main().catch((e: Error) => {
  rmSync(home, { recursive: true, force: true });
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
