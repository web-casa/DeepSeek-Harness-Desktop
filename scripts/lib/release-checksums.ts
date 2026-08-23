// GitHub Release uploads normalize spaces in an asset filename to dots. Keep
// the name recorded in a SHA-256 sidecar aligned with the *published* asset,
// not its local staging filename. The draft-release audit below independently
// verifies this contract against the API before a release becomes public.

import { basename } from "node:path";

export interface GithubReleaseAsset {
  id: number;
  name: string;
  digest?: string | null;
}

export interface ParsedSha256Sidecar {
  digest: string;
  filename: string;
}

function isSafeAssetBasename(name: string): boolean {
  return (
    name.length > 0 &&
    name === basename(name) &&
    !name.startsWith(".") &&
    !/[\\/:\u0000-\u001f\u007f]/.test(name) &&
    name.trim() === name
  );
}

/**
 * Return the reviewed name exposed by the GitHub Release upload action.
 *
 * The build emits local Tauri filenames such as `DSH Desktop_…`; GitHub's
 * upload path exposes them as `DSH.Desktop_…`. Other whitespace is rejected
 * rather than guessed, and the draft audit remains the authoritative final
 * check in case the host changes its normalization behavior.
 */
export function githubReleaseAssetName(localName: string): string {
  if (!isSafeAssetBasename(localName) || /\s/.test(localName.replaceAll(" ", ""))) {
    throw new Error(`unsafe local release asset filename: ${JSON.stringify(localName)}`);
  }
  return localName.replaceAll(" ", ".");
}

export function sha256SidecarContent(digest: string, localAssetName: string): string {
  if (!/^[a-f0-9]{64}$/.test(digest)) {
    throw new Error("SHA-256 digest must be 64 lowercase hexadecimal characters");
  }
  return `${digest}  ${githubReleaseAssetName(localAssetName)}\n`;
}

export function parseSha256Sidecar(content: string): ParsedSha256Sidecar | null {
  const match = /^([a-f0-9]{64})  ([^\r\n]+)\r?\n$/.exec(content);
  if (!match || !isSafeAssetBasename(match[2])) return null;
  return { digest: match[1], filename: match[2] };
}

/**
 * Validate all public installer / SHA-256 pairs visible on a draft release.
 * `sidecarContents` maps an asset id to its downloaded text content.
 */
export function releaseChecksumProblems(
  installers: readonly GithubReleaseAsset[],
  sidecars: readonly GithubReleaseAsset[],
  sidecarContents: ReadonlyMap<number, string>,
): string[] {
  const problems: string[] = [];
  const installerByName = new Map<string, GithubReleaseAsset>();
  const sidecarByName = new Map<string, GithubReleaseAsset>();

  for (const installer of installers) {
    if (installerByName.has(installer.name)) {
      problems.push(`duplicate public installer asset: ${installer.name}`);
    }
    installerByName.set(installer.name, installer);
  }
  for (const sidecar of sidecars) {
    if (sidecarByName.has(sidecar.name)) {
      problems.push(`duplicate SHA-256 sidecar asset: ${sidecar.name}`);
    }
    sidecarByName.set(sidecar.name, sidecar);
  }

  for (const installer of installers) {
    const expectedSidecarName = `${installer.name}.sha256`;
    const sidecar = sidecarByName.get(expectedSidecarName);
    if (!sidecar) {
      problems.push(`missing SHA-256 sidecar for published asset: ${installer.name}`);
      continue;
    }
    const parsed = parseSha256Sidecar(sidecarContents.get(sidecar.id) ?? "");
    if (!parsed) {
      problems.push(`malformed SHA-256 sidecar: ${sidecar.name}`);
      continue;
    }
    if (parsed.filename !== installer.name) {
      problems.push(
        `SHA-256 sidecar filename mismatch: ${sidecar.name} names ${parsed.filename}, published asset is ${installer.name}`,
      );
    }
    const githubDigest = installer.digest;
    if (!githubDigest || githubDigest !== `sha256:${parsed.digest}`) {
      problems.push(
        `SHA-256 sidecar digest mismatch: ${sidecar.name} has sha256:${parsed.digest}, GitHub reports ${githubDigest ?? "none"} for ${installer.name}`,
      );
    }
  }

  for (const sidecar of sidecars) {
    const installerName = sidecar.name.endsWith(".sha256")
      ? sidecar.name.slice(0, -".sha256".length)
      : "";
    if (!installerByName.has(installerName)) {
      problems.push(`orphan SHA-256 sidecar on public release: ${sidecar.name}`);
    }
  }
  return problems;
}
