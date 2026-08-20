import { join } from "node:path";
import { repoRoot } from "./common.ts";
import type { ReleaseArch } from "./release-artifacts.ts";

export interface AscCliDistribution {
  file: string;
  sha256: string;
}

// Reviewed 2026-08-20 from the immutable GitHub Release assets. The workflow
// never executes a moving Homebrew formula or install script.
export const ascCliRelease = {
  version: "4.6.0",
  baseUrl: "https://github.com/rorkai/App-Store-Connect-CLI/releases/download/4.6.0/",
  distributions: {
    x64: {
      file: "asc_4.6.0_macOS_amd64",
      sha256: "0eb9544221fa8615232415a89e3084483c12556bb86066cba304af80343cc905",
    },
    arm64: {
      file: "asc_4.6.0_macOS_arm64",
      sha256: "91c57dd01c5c7c10d3fcf894268ae8202ebf1b005aca3bde29637ec9eb7bd656",
    },
  } satisfies Record<ReleaseArch, AscCliDistribution>,
} as const;

export function ascCliDistribution(arch: ReleaseArch): AscCliDistribution {
  return ascCliRelease.distributions[arch];
}

export function ascCliDownloadUrl(arch: ReleaseArch): URL {
  const distribution = ascCliDistribution(arch);
  return new URL(distribution.file, ascCliRelease.baseUrl);
}

export function ascCliPath(): string {
  return join(repoRoot, ".tmp", "release-tools", "asc");
}

export function ascCliDefinitionProblems(): string[] {
  const problems: string[] = [];
  for (const arch of ["x64", "arm64"] as const) {
    const distribution = ascCliDistribution(arch);
    const url = ascCliDownloadUrl(arch);
    if (url.protocol !== "https:" || url.hostname !== "github.com" || url.port !== "") {
      problems.push(`${arch}: ASC CLI source must be github.com HTTPS`);
    }
    if (
      url.pathname !==
      `/rorkai/App-Store-Connect-CLI/releases/download/${ascCliRelease.version}/${distribution.file}`
    ) {
      problems.push(`${arch}: ASC CLI release URL is not version-bound`);
    }
    if (!/^[a-f0-9]{64}$/.test(distribution.sha256)) {
      problems.push(`${arch}: ASC CLI SHA-256 is invalid`);
    }
  }
  return problems;
}
