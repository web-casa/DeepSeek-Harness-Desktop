import { join } from "node:path";
import type { ReleaseArch } from "./release-artifacts.ts";

export const DMG_CREATE_TIMEOUT_MS = 10 * 60 * 1000;
export const DMG_TOOL_TIMEOUT_MS = 5 * 60 * 1000;

function safeFileComponent(value: string, label: string): string {
  const trimmed = value.trim();
  if (
    trimmed !== value ||
    trimmed.length === 0 ||
    trimmed === "." ||
    trimmed === ".." ||
    trimmed.includes("/") ||
    trimmed.includes("\\") ||
    trimmed.includes("\0")
  ) {
    throw new Error(`${label} is not a safe filename component`);
  }
  return trimmed;
}

export function dmgArtifactName(
  productName: string,
  version: string,
  arch: ReleaseArch,
): string {
  if (arch !== "x64" && arch !== "arm64") {
    throw new Error(`unsupported DMG architecture: ${String(arch)}`);
  }
  const safeProductName = safeFileComponent(productName, "product name");
  const safeVersion = safeFileComponent(version, "version");
  const artifactArch = arch === "arm64" ? "aarch64" : "x64";
  return `${safeProductName}_${safeVersion}_${artifactArch}.dmg`;
}

export function macosDmgPaths(
  root: string,
  productName: string,
  version: string,
  arch: ReleaseArch,
): { appDirectory: string; outputDirectory: string; output: string } {
  const appDirectory = join(root, "target", "release", "bundle", "macos");
  const outputDirectory = join(root, "target", "release", "bundle", "dmg");
  return {
    appDirectory,
    outputDirectory,
    output: join(outputDirectory, dmgArtifactName(productName, version, arch)),
  };
}
