// Pure updater-manifest assembly helpers. Keeping filename selection here
// lets the release command and tests share one exact architecture contract.

import {
  WINDOWS_NSIS_UPDATER_PLATFORMS,
  type WindowsNsisUpdaterPlatform,
} from "./release-artifacts.ts";

export interface UpdaterAsset {
  id: number;
  name: string;
}

export interface UpdaterManifestEntry {
  signature: string;
  url: string;
}

const ARTIFACT_PATTERNS: Readonly<
  Record<WindowsNsisUpdaterPlatform, readonly RegExp[]>
> = {
  "windows-x86_64-nsis": [
    /(?:^|[_\-.])(?:x64|x86_64)-setup\.exe$/i,
    /(?:^|[_\-.])(?:x64|x86_64)(?:-setup)?\.nsis\.zip$/i,
    /(?:^|[_\-.])(?:x64|x86_64)-setup\.zip$/i,
  ],
  "windows-aarch64-nsis": [
    /(?:^|[_\-.])(?:arm64|aarch64)-setup\.exe$/i,
    /(?:^|[_\-.])(?:arm64|aarch64)(?:-setup)?\.nsis\.zip$/i,
    /(?:^|[_\-.])(?:arm64|aarch64)-setup\.zip$/i,
  ],
};

export function isWindowsNsisUpdaterPlatform(value: string): value is WindowsNsisUpdaterPlatform {
  return (WINDOWS_NSIS_UPDATER_PLATFORMS as readonly string[]).includes(value);
}

/**
 * Pair one exact architecture's NSIS update payload with its detached
 * signature. A generic `-setup.exe` match is intentionally forbidden: a
 * mixed x64/ARM64 release must never publish the first asset it happens to
 * enumerate under the wrong updater target.
 */
export function platformArtifactFor(
  assets: readonly UpdaterAsset[],
  platform: WindowsNsisUpdaterPlatform,
): { artifact: UpdaterAsset; sig: UpdaterAsset } | null {
  for (const pattern of ARTIFACT_PATTERNS[platform]) {
    const artifact = assets.find((asset) => pattern.test(asset.name) && !asset.name.endsWith(".sig"));
    if (!artifact) continue;
    const sig = assets.find((asset) => asset.name === `${artifact.name}.sig`);
    if (sig) return { artifact, sig };
  }
  return null;
}

/** Pure assembly of the Tauri static-release manifest. */
export function assembleLatestJson(
  version: string,
  notes: string,
  pubDate: string,
  platforms: Readonly<Record<string, UpdaterManifestEntry>>,
): string {
  return `${JSON.stringify({ version, notes, pub_date: pubDate, platforms }, null, 2)}\n`;
}

/**
 * Build a canonical GitHub release-download URL from API-returned asset
 * metadata.  Release assets are allowed to contain spaces (the reviewed
 * Windows installer names do), so interpolation without segment encoding can
 * leave a manifest that some HTTP stacks reject or reinterpret.
 */
export function githubReleaseAssetUrl(repo: string, tag: string, assetName: string): string {
  const segments = repo.split("/");
  if (
    segments.length !== 2 ||
    segments.some((segment) => !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(segment)) ||
    !tag ||
    tag.includes("/") ||
    !assetName ||
    /[\\/\0]/.test(assetName)
  ) {
    throw new Error("invalid GitHub release asset identity");
  }
  return `https://github.com/${segments.map(encodeURIComponent).join("/")}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(assetName)}`;
}
