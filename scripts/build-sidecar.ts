// Build the dsh-sidecar supervisor for the current host (or --target) and
// stage it into src-tauri/resources/runtime/sidecar[.exe].

import { existsSync, chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { repoRoot, runtimeDir, sidecarPath, fail, ok, info } from "./lib/common.ts";

const targetFlag = process.argv.indexOf("--target");
const target = targetFlag >= 0 ? process.argv[targetFlag + 1] : undefined;

function hostTriple(): string {
  const res = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  if (res.status !== 0) fail("rustc not available");
  const line = res.stdout.split("\n").find((l) => l.startsWith("host: "));
  if (!line) fail("could not determine host triple");
  return line.slice("host: ".length).trim();
}

const triple = target ?? hostTriple();
info(`building dsh-sidecar for ${triple}`);

const args = [
  "build",
  "--release",
  "--target",
  triple,
  "--manifest-path",
  join(repoRoot, "crates/dsh-sidecar/Cargo.toml"),
];
const res = spawnSync("cargo", args, { stdio: "inherit" });
if (res.status !== 0) fail("cargo build failed");

const exe = process.platform === "win32" || triple.includes("windows") ? ".exe" : "";
const built = join(repoRoot, "crates/dsh-sidecar/target", triple, "release", `dsh-sidecar${exe}`);
if (!existsSync(built)) fail(`built binary missing at ${built}`);

mkdirSync(runtimeDir, { recursive: true });
copyFileSync(built, sidecarPath());
if (exe === "") chmodSync(sidecarPath(), 0o755);

const probe = spawnSync(sidecarPath(), [], { input: '{"command":"status"}\n', encoding: "utf8", timeout: 5000 });
ok(`dsh-sidecar staged at ${sidecarPath()}`);
info(`probe reply: ${(probe.stdout ?? probe.stderr ?? "").trim().split("\n").pop()}`);
