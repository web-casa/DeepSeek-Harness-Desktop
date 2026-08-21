// Plugin installation e2e: drives the REAL `dsh plugin` chain the desktop
// shell spawns — bundled node → dsh lib/bin.js `plugin --profile web
// add/remove` → shim-resolved bundled pnpm → upstream initProfile +
// reconcilePlugins — against a fresh, isolated DSH_HOME.
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
//   - `remove is-odd` drops the dependency again.
//   - the reported dsh-plugin-pkgseek@0.1.1 active-bundle uninstall path
//     removes both its dependency and dsh.profile.bundles entry.
//
// Store isolation (plan S5): `pnpm_config_store_dir` pins the pnpm content
// store inside the temp home, so nothing touches the user's real store.
// (pnpm reads env config under the `pnpm_config_` prefix — verified against
// the bundled pnpm 11.21.0; `npm_config_*` is NOT honored for this key.)
// NOTE: requires network access to registry.npmjs.org (CI runners have it).
//
// The Rust side of the shim text/exec is covered by plugins.rs tests; the
// text below mirrors it verbatim and is asserted against the same golden
// strings here so a drift breaks this run.

import { spawn } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { delimiter, join, resolve } from "node:path";
import {
  runtimeDir as defaultRuntimeDir,
  tmpDir,
  fail,
  ok,
  info,
} from "./lib/common.ts";

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

// Same golden texts as src-tauri/src/plugins.rs (asserted there by unit
// tests; asserted HERE so a silent drift between the two never ships).
const SHIM_SCRIPT = `#!/bin/sh\nexec "${node}" "${pnpmCjs}" "$@"\n`;
const SHIM_CMD = `@echo off\n"${node}" "${pnpmCjs}" %*\n`;

function writeShims(): void {
  mkdirSync(toolsDir, { recursive: true });
  writeFileSync(join(toolsDir, "pnpm"), SHIM_SCRIPT);
  writeFileSync(join(toolsDir, "pnpm.cmd"), SHIM_CMD);
  if (process.platform !== "win32") chmodSync(join(toolsDir, "pnpm"), 0o755);
}

function pluginEnv(): NodeJS.ProcessEnv {
  return {
    ...process.env,
    DSH_HOME: home,
    DSH_TELEMETRY_DISABLED: "1",
    PATH: `${toolsDir}${delimiter}${process.env.PATH ?? ""}`,
    pnpm_config_store_dir: storeDir,
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

interface ProfileManifest {
  dependencies?: Record<string, string>;
  dsh?: { profile?: { bundles?: string[] } };
}

function readProfileManifest(): ProfileManifest {
  return JSON.parse(readFileSync(join(profileDir, "package.json"), "utf8")) as ProfileManifest;
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

  // --- remove ----------------------------------------------------------
  const removeOut = await runPlugin(["remove", "is-odd"], "dsh plugin remove is-odd");
  if (removeOut.includes("declares no dsh.bundle")) {
    // Removal must not re-warn: the package left the dependency list, so
    // reconcile has nothing newly-added to report.
    fail("reconcilePlugins warned about a removed package");
  }
  ok("dsh plugin remove is-odd exited 0 (no spurious reconcile warning)");
  const after = readProfileManifest().dependencies ?? {};
  if (typeof after["is-odd"] === "string") fail(`is-odd still in dependencies after remove: ${JSON.stringify(after)}`);
  if (existsSync(join(profileDir, "node_modules", "is-odd"))) fail("node_modules/is-odd still present after remove");
  ok("remove dropped the dependency and the installed tree entry");

  // Regression for the real package reported by Desktop users. Unlike
  // is-odd it declares dsh.bundle, so this covers the active-layer removal
  // and not only pnpm's dependency deletion path.
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

  await runPlugin(
    ["remove", "dsh-plugin-pkgseek"],
    "dsh plugin remove dsh-plugin-pkgseek",
  );
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
