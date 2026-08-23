// Native release jobs are deliberately tied to a hosted runner with the same
// operating system and CPU architecture as the packaged Desktop payload.
// Keep this pure so the release matrix contract can be tested without a
// platform-specific runner.

import type { NativeReleaseTarget } from "./release-artifacts.ts";

export interface NativeHostObservation {
  platform: NodeJS.Platform;
  arch: NodeJS.Architecture;
  rustHost: string;
}

function expectedPlatform(target: NativeReleaseTarget): NodeJS.Platform {
  if (target.id.startsWith("windows-")) return "win32";
  if (target.id.startsWith("macos-")) return "darwin";
  if (target.id.startsWith("linux-")) return "linux";
  throw new Error(`target has no reviewed platform family: ${target.id}`);
}

export function nativeHostProblems(
  target: NativeReleaseTarget,
  observation: NativeHostObservation,
): string[] {
  const problems: string[] = [];
  const platform = expectedPlatform(target);
  if (observation.platform !== platform) {
    problems.push(`${target.id}: expected Node platform ${platform}, got ${observation.platform}`);
  }
  if (observation.arch !== target.arch) {
    problems.push(`${target.id}: expected Node architecture ${target.arch}, got ${observation.arch}`);
  }
  if (observation.rustHost !== target.hostTriple) {
    problems.push(
      `${target.id}: expected Rust host ${target.hostTriple}, got ${observation.rustHost || "missing"}`,
    );
  }
  return problems;
}
