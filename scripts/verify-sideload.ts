// Sideload e2e: drives the REAL `dsh plugin --profile web add file:<abs>.tgz`
// chain that `sideload_plugin` uses after stage_sideload(). Proves the
// file: spec survives the dsh -> pnpm shell boundary on the current platform.

import { spawn, spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { runtimeDir as defaultRuntimeDir, tmpDir, fail, ok, info } from "./lib/common.ts";
import { pluginPath, pluginRemoveArgs, writePluginShims } from "./lib/plugin-shim.ts";

const runtimeDirArg = process.argv.indexOf("--runtime-dir");
if (runtimeDirArg >= 0 && !process.argv[runtimeDirArg + 1]) {
  fail("--runtime-dir requires a path");
}
const rtDir = runtimeDirArg >= 0 ? resolve(process.argv[runtimeDirArg + 1]) : defaultRuntimeDir;
const harness = join(rtDir, "harness");
const node = join(rtDir, `node${process.platform === "win32" ? ".exe" : ""}`);
const dshBin = join(harness, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
const pnpmCjs = join(harness, "node_modules", "pnpm", "bin", "pnpm.cjs");

for (const [file, label] of [
  [node, "bundled node"],
  [dshBin, "dsh entry"],
  [pnpmCjs, "bundled pnpm"],
] as const) {
  if (!existsSync(file)) fail(`${label} missing at ${file} — run runtime:all first`);
}

const home = join(tmpDir, `sideload-e2e-home-${Date.now()}`);
const fixture = join(tmpDir, `sideload-fixture-${Date.now()}`);
const packageDir = join(fixture, "package");
const tarball = join(fixture, "fixture.tgz");
const toolsDir = join(home, ".desktop-tools");
const profileDir = join(home, "profiles", "web");
const storeDir = join(home, "pnpm-store");
const configDir = join(toolsDir, "pnpm-config");
const uninstallScriptMarker = join(home, "uninstall-script-ran");

function runChild(script: string, args: string[], cwd: string, label: string): Promise<string> {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(node, [script, ...args], {
      cwd,
      env: {
        ...process.env,
        DSH_HOME: home,
        DSH_TELEMETRY_DISABLED: "1",
        PATH: pluginPath(toolsDir, process.env.PATH),
        pnpm_config_store_dir: storeDir,
      },
      stdio: ["ignore", "pipe", "pipe"],
      detached: process.platform !== "win32",
    });
    let output = "";
    const collect = (c: Buffer) => {
      output += c.toString("utf8");
    };
    child.stdout.on("data", collect);
    child.stderr.on("data", collect);
    const timer = setTimeout(() => {
      if (process.platform === "win32") {
        spawnSync("taskkill", ["/pid", String(child.pid), "/T", "/F"]);
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

function run(args: string[], label: string): Promise<string> {
  return runChild(dshBin, ["plugin", "--profile", "web", ...args], harness, label);
}

function runPnpmRemove(name: string, label: string): Promise<string> {
  const modulesState = JSON.parse(
    readFileSync(join(profileDir, "node_modules", ".modules.yaml"), "utf8"),
  ) as { storeDir?: unknown };
  if (typeof modulesState.storeDir !== "string") {
    fail("pnpm .modules.yaml did not record a string storeDir");
  }
  return runChild(
    pnpmCjs,
    pluginRemoveArgs(name, configDir, dirname(modulesState.storeDir)),
    profileDir,
    label,
  );
}

function makeFixture(): void {
  rmSync(fixture, { recursive: true, force: true });
  mkdirSync(packageDir, { recursive: true });
  writeFileSync(
    join(packageDir, "package.json"),
    JSON.stringify({
      name: "dsh-sideload-fixture",
      version: "1.0.0",
      scripts: { preuninstall: "node uninstall-marker.cjs" },
    }),
  );
  writeFileSync(
    join(packageDir, "uninstall-marker.cjs"),
    `require("node:fs").writeFileSync(${JSON.stringify(uninstallScriptMarker)}, "unsafe");\n`,
  );
  const res = spawnSync("tar", ["-czf", tarball, "-C", fixture, "package"], {
    encoding: "utf8",
  });
  if (res.status !== 0) {
    fail(`tar failed: ${res.stderr}`);
  }
}

async function main(): Promise<void> {
  rmSync(home, { recursive: true, force: true });
  writePluginShims(toolsDir, node, pnpmCjs);
  mkdirSync(configDir, { recursive: true });
  writeFileSync(join(configDir, ".npmrc"), "");
  makeFixture();
  info(`isolated DSH_HOME ${home} · tarball ${tarball}`);

  const spec = `file:${tarball}`;
  await run(["add", spec], `dsh plugin add ${spec}`);
  ok("dsh plugin add file:<fixture>.tgz exited 0");

  const manifest = JSON.parse(readFileSync(join(profileDir, "package.json"), "utf8")) as {
    dependencies?: Record<string, string>;
  };
  const deps = manifest.dependencies ?? {};
  if (typeof deps["dsh-sideload-fixture"] !== "string") {
    fail(`sideload fixture missing from profile dependencies: ${JSON.stringify(deps)}`);
  }
  // pnpm normalizes file: specs to forward slashes on Windows.
  const expectedFileSpec = `file:${tarball.replace(/\\/g, "/")}`;
  if (deps["dsh-sideload-fixture"] !== expectedFileSpec) {
    fail(`dependency spec mismatch: ${deps["dsh-sideload-fixture"]} !== ${expectedFileSpec}`);
  }
  ok(`profile dependency recorded exactly as ${deps["dsh-sideload-fixture"]}`);

  // The desktop keeps a successfully installed file: tarball around because
  // the profile dependency still references it. Prove a later pnpm operation
  // can still resolve that path before the package is removed.
  await run(["add", "is-odd"], "dsh plugin add is-odd while file: dep is retained");
  ok("subsequent add succeeded while file: dependency source was retained");

  await runPnpmRemove("dsh-sideload-fixture", "direct pnpm remove dsh-sideload-fixture");
  if (existsSync(uninstallScriptMarker)) {
    fail("direct plugin removal executed the package preuninstall lifecycle script");
  }
  ok("direct, scripts-disabled pnpm remove dsh-sideload-fixture exited 0");

  rmSync(home, { recursive: true, force: true });
  rmSync(fixture, { recursive: true, force: true });
  console.log("\n  PASS — sideload file: install e2e complete");
}

main().catch((e: Error) => {
  rmSync(home, { recursive: true, force: true });
  rmSync(fixture, { recursive: true, force: true });
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
