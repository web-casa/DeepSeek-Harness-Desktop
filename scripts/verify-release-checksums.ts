// Validate SHA-256 sidecars against the assets GitHub actually accepted on a
// draft Release. This is intentionally after upload: local paths cannot see
// upload-time filename normalization.

import { spawnSync } from "node:child_process";
import { fail, info, ok } from "./lib/common.ts";
import {
  releaseChecksumProblems,
  type GithubReleaseAsset,
} from "./lib/release-checksums.ts";
import { classifyPublicInstaller, publicInstallerInventoryProblems } from "./lib/release-inventory.ts";

interface ReleaseInfo {
  tag_name: string;
  assets: GithubReleaseAsset[];
}

function parseJson<T>(text: string, context: string): T {
  try {
    return JSON.parse(text) as T;
  } catch (error) {
    fail(`${context} returned invalid JSON: ${(error as Error).message}`);
  }
}

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function isRepositoryName(value: string): boolean {
  return /^[^/\s]+\/[^/\s]+$/.test(value);
}

function resolveRepo(): string {
  if (process.env.GITHUB_REPOSITORY) {
    if (!isRepositoryName(process.env.GITHUB_REPOSITORY)) {
      fail("GITHUB_REPOSITORY must be an owner/name repository identity");
    }
    return process.env.GITHUB_REPOSITORY;
  }
  const result = spawnSync("gh", ["repo", "view", "--json", "nameWithOwner"], {
    encoding: "utf8",
  });
  if (result.status !== 0) {
    fail(`could not determine repository: ${result.stderr || result.error?.message || "gh repo view failed"}`);
  }
  const name = parseJson<{ nameWithOwner?: unknown }>(
    result.stdout || "{}",
    "gh repo view",
  ).nameWithOwner;
  if (typeof name !== "string" || !isRepositoryName(name)) {
    fail("could not determine repository (set GITHUB_REPOSITORY)");
  }
  return name;
}

function ghJson<T>(path: string): T {
  const result = spawnSync("gh", ["api", path], { encoding: "utf8" });
  if (result.status !== 0) {
    fail(`gh api ${path} failed: ${result.stderr || result.error?.message || "unknown error"}`);
  }
  return parseJson<T>(result.stdout ?? "", `gh api ${path}`);
}

function ghAssetText(repo: string, asset: GithubReleaseAsset): string {
  const result = spawnSync(
    "gh",
    ["api", `repos/${repo}/releases/assets/${asset.id}`, "-H", "Accept: application/octet-stream"],
    { encoding: "utf8" },
  );
  if (result.status !== 0) {
    fail(`could not download SHA-256 sidecar ${asset.name}: ${result.stderr}`);
  }
  return result.stdout ?? "";
}

const tag = argument("--tag");
if (!tag || !/^v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$/.test(tag)) {
  fail("usage: node scripts/verify-release-checksums.ts --tag vMAJOR.MINOR.PATCH");
}

const repo = resolveRepo();
// Draft releases cannot reliably be fetched through releases/tags/<tag>.
const releases = ghJson<ReleaseInfo[]>(`repos/${repo}/releases?per_page=100`);
if (!Array.isArray(releases)) fail("GitHub releases API returned a non-array response");
const release = releases.find((candidate) => candidate.tag_name === tag);
if (!release) fail(`release ${tag} not found (${releases.length} releases listed)`);
if (!Array.isArray(release.assets)) fail(`release ${tag} has an invalid asset list`);
for (const [index, asset] of release.assets.entries()) {
  if (
    !Number.isSafeInteger(asset.id) ||
    asset.id <= 0 ||
    typeof asset.name !== "string" ||
    (asset.digest !== undefined && asset.digest !== null && typeof asset.digest !== "string")
  ) {
    fail(`release ${tag} has an invalid asset at index ${index}`);
  }
}

const installers = release.assets.filter((asset) => classifyPublicInstaller(asset.name) !== null);
const sidecars = release.assets.filter((asset) => asset.name.endsWith(".sha256"));
const sidecarContents = new Map(
  sidecars.map((sidecar) => [sidecar.id, ghAssetText(repo, sidecar)]),
);
const problems = [
  ...publicInstallerInventoryProblems(installers.map((installer) => installer.name)),
  ...releaseChecksumProblems(installers, sidecars, sidecarContents),
];
if (problems.length > 0) {
  fail(`draft Release checksum contract failed:\n- ${problems.join("\n- ")}`);
}
for (const installer of installers) info(`draft checksum verified: ${installer.name}`);
ok(`draft Release ${tag} checksum sidecars match ${installers.length} published installers`);
