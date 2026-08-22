// Fail closed if a release matrix row has drifted from a native hosted runner.
// This protects the bundled Node, sidecar and Desktop binary from quietly
// becoming cross-compiled or emulated artifacts.

import { spawnSync } from "node:child_process";
import { fail, ok } from "./lib/common.ts";
import { nativeHostProblems } from "./lib/native-host.ts";
import { targetById } from "./lib/release-artifacts.ts";

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function rustHostTriple(): string {
  const result = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  if (result.status !== 0) {
    fail(`rustc -vV failed: ${(result.stderr ?? result.stdout ?? "no output").trim()}`);
  }
  const host = /^host:\s*(\S+)\s*$/m.exec(result.stdout ?? "")?.[1];
  if (!host) fail("rustc -vV did not report a host triple");
  return host;
}

const targetId = argument("--target");
if (!targetId) fail("usage: node scripts/verify-native-host.ts --target <matrix-target>");
const target = targetById(targetId);
if (!target) fail(`unknown native release target: ${targetId}`);

const rustHost = rustHostTriple();
const problems = nativeHostProblems(target, {
  platform: process.platform,
  arch: process.arch,
  rustHost,
});
if (problems.length > 0) fail(`native host validation failed:\n- ${problems.join("\n- ")}`);
ok(`${target.id} is native: ${process.platform}/${process.arch}, ${rustHost}`);
