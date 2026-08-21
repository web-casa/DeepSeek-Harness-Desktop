import assert from "node:assert/strict";
import test from "node:test";
import { delimiter } from "node:path";
import {
  pluginPath,
  pluginMarketAddArgs,
  pluginRemoveArgs,
  pluginShimCmd,
  pluginShimScript,
} from "./plugin-shim.ts";

test("plugin smoke shim matches the production quoting contract", () => {
  assert.equal(
    pluginShimScript("/tmp/a'b/$node", "/tmp/$(touch nope)/pnpm.cjs"),
    `#!/bin/sh\nexec '/tmp/a'"'"'b/$node' '/tmp/$(touch nope)/pnpm.cjs' "$@"\n`,
  );
  assert.equal(
    pluginShimCmd("C:\\100%\\node.exe", "C:\\bang!\\pnpm.cjs"),
    '@echo off\nsetlocal DisableDelayedExpansion\n"C:\\100%%\\node.exe" "C:\\bang!\\pnpm.cjs" %*\n',
  );
});

test("plugin smoke PATH preserves every inherited segment", () => {
  const inherited = ["parent-one", "parent-two"].join(delimiter);
  assert.deepEqual(pluginPath("desktop-shim", inherited).split(delimiter), [
    "desktop-shim",
    "parent-one",
    "parent-two",
  ]);
  assert.equal(pluginPath("desktop-shim", undefined), "desktop-shim");
});

test("plugin smoke uninstall mirrors the direct safe pnpm contract", () => {
  const args = pluginRemoveArgs("fixture-plugin", "/private/config", "/existing/store");
  assert.deepEqual(args.slice(0, 2), ["remove", "fixture-plugin"]);
  for (const required of [
    "--config.ignore-scripts=true",
    "--config.ignore-pnpmfile=true",
    "--config.enable-global-virtual-store=false",
    "--config.verify-store-integrity=true",
    "--config.strict-store-pkg-content-check=true",
    "--config.config-dir=/private/config",
    "--config.userconfig=/private/config/.npmrc",
    "--config.globalconfig=/private/config/.npmrc",
    "--store-dir=/existing/store",
  ]) {
    assert.ok(args.includes(required), `missing ${required}`);
  }
  assert.ok(!args.includes("--ignore-scripts"));
});

test("plugin smoke market install keeps scripts disabled and source exact", () => {
  const args = pluginMarketAddArgs(
    "https://registry.npmjs.org/fixture/-/fixture-1.0.0.tgz",
    "https://registry.npmjs.org",
    "/private/config",
    "/existing/store",
  );
  assert.deepEqual(args.slice(0, 2), [
    "add",
    "https://registry.npmjs.org/fixture/-/fixture-1.0.0.tgz",
  ]);
  for (const required of [
    "--ignore-scripts",
    "--config.ignore-pnpmfile=true",
    "--save-exact",
    "--registry=https://registry.npmjs.org",
    "--store-dir=/existing/store",
  ]) {
    assert.ok(args.includes(required), `missing ${required}`);
  }
});
