// Build the Tauri updater's latest.json AFTER the GitHub Release exists.
//
// Why a separate step: `tauri build` only signs updater bundles (.sig files);
// the manifest itself must reference ABSOLUTE asset URLs that exist only once
// the release is created, and the signature field is the CONTENT of the .sig
// asset. Runs in the publish job (gh CLI + GITHUB_TOKEN).
//
// Fail-closed: if a requested platform has no matching artifact+sig pair, the
// script fails the job — a release must never go out with a broken manifest.
//
//   node scripts/updater-manifest.ts --tag v0.2.2 --platforms windows-x86_64-nsis,windows-aarch64-nsis

import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { fail, ok, info } from "./lib/common.ts";
import {
  assembleLatestJson,
  githubReleaseAssetUrl,
  isWindowsNsisUpdaterPlatform,
  platformArtifactFor,
  type UpdaterAsset,
} from "./lib/updater-manifest.ts";
import { publishedWindowsNsisUpdaterPlatforms } from "./lib/release-artifacts.ts";

export interface ReleaseInfo {
  tag_name: string;
  assets: UpdaterAsset[];
  body: string | null;
}

function resolveRepo(): string {
  if (process.env.GITHUB_REPOSITORY) return process.env.GITHUB_REPOSITORY;
  const r = spawnSync("gh", ["repo", "view", "--json", "nameWithOwner"], { encoding: "utf8" });
  const name = JSON.parse(r.stdout || "{}").nameWithOwner as string | undefined;
  if (!name) fail("could not determine repository (set GITHUB_REPOSITORY)");
  return name;
}

function ghApi(path: string): { status: number | null; stdout: string; error?: Error } {
  const res = spawnSync("gh", ["api", path, "-H", "Accept: application/octet-stream"], {
    encoding: "utf8",
  });
  return { status: res.status, stdout: res.stdout ?? "", error: res.error };
}

function ghJson<T>(path: string): T {
  const res = spawnSync("gh", ["api", path], { encoding: "utf8" });
  if (res.status !== 0) fail(`gh api ${path} failed: ${res.stderr}`);
  return JSON.parse(res.stdout ?? "") as T;
}

function runSelfTest(): void {
  const fixtures: UpdaterAsset[] = [
    { id: 1, name: "DeepSeek.Harness.Desktop_0.2.2_x64-setup.exe" },
    { id: 2, name: "DeepSeek.Harness.Desktop_0.2.2_x64-setup.exe.sig" },
    { id: 3, name: "DeepSeek.Harness.Desktop_0.2.2_x64-setup.exe.sha256" },
    { id: 4, name: "DeepSeek.Harness.Desktop_0.2.2_arm64-setup.exe" },
    { id: 5, name: "DeepSeek.Harness.Desktop_0.2.2_arm64-setup.exe.sig" },
  ];
  const pair = platformArtifactFor(fixtures, "windows-x86_64-nsis");
  if (!pair || pair.artifact.name !== fixtures[0].name || pair.sig.name !== fixtures[1].name) {
    fail("self-test: windows artifact pairing wrong");
  }
  if (platformArtifactFor(fixtures, "windows-aarch64-nsis")?.artifact.name !== fixtures[3].name) {
    fail("self-test: ARM64 artifact pairing wrong");
  }
  const doc = assembleLatestJson(
    "0.2.2",
    "",
    "2026-08-16T00:00:00Z",
    { "windows-x86_64-nsis": { signature: "SIG", url: "https://example/x64-setup.exe" } },
  );
  const parsed = JSON.parse(doc);
  if (parsed.platforms["windows-x86_64-nsis"].signature !== "SIG") {
    fail("self-test: manifest assembly wrong");
  }
  ok("self-test: updater-manifest pairing + assembly");
}

const tagIdx = process.argv.indexOf("--tag");
const platsIdx = process.argv.indexOf("--platforms");
if (process.argv.includes("--self-test")) {
  runSelfTest();
  process.exit(0);
}
const tag = tagIdx >= 0 ? process.argv[tagIdx + 1] : undefined;
const platforms = platsIdx >= 0 ? (process.argv[platsIdx + 1] ?? "").split(",") : [];
if (!tag || platforms.length === 0) {
  fail("usage: node scripts/updater-manifest.ts --tag vX.Y.Z --platforms windows-x86_64-nsis[,windows-aarch64-nsis] [--self-test]");
}
for (const platform of platforms) {
  if (!isWindowsNsisUpdaterPlatform(platform)) {
    fail(`unreviewed updater platform: ${platform}`);
  }
}
const expectedPlatforms = publishedWindowsNsisUpdaterPlatforms();
if (
  platforms.length !== expectedPlatforms.length ||
  platforms.some((platform, index) => platform !== expectedPlatforms[index])
) {
  fail(
    `updater platform set drifted from the release plan: got ${platforms.join(",")}, expected ${expectedPlatforms.join(",")}`,
  );
}
const repo = resolveRepo();

// `releases/tags/<tag>` 404s for DRAFT releases (API quirk) — the list
// endpoint is the only reliable lookup right after softprops creates one.
const releases = ghJson<ReleaseInfo[]>(`repos/${repo}/releases?per_page=100`);
const release = releases.find((r) => r.tag_name === tag);
if (!release) {
  fail(`release ${tag} not found (${releases.length} releases listed)`);
}
const version = tag.replace(/^v/, "");
const entries: Record<string, { signature: string; url: string }> = {};
for (const platform of platforms) {
  // The validation above narrows this runtime string before we touch the
  // architecture-specific matching table.
  if (!isWindowsNsisUpdaterPlatform(platform)) fail(`unreviewed updater platform: ${platform}`);
  const pair = platformArtifactFor(release.assets, platform);
  if (!pair) {
    fail(`no updater artifact+sig pair for ${platform} on release ${tag} — refusing to publish a broken manifest`);
  }
  const sig = ghApi(`repos/${repo}/releases/assets/${pair.sig.id}`);
  if (sig.status !== 0 || !sig.stdout.trim()) {
    fail(`could not read .sig content for ${pair.sig.name} (${sig.error?.message ?? ""})`);
  }
  const url = githubReleaseAssetUrl(repo, tag, pair.artifact.name);
  entries[platform] = { signature: sig.stdout.trim(), url };
  info(`${platform}: ${pair.artifact.name} (sig ${pair.sig.name})`);
}
if (Object.keys(entries).length === 0) {
  fail("no platforms matched — nothing to publish");
}
const doc = assembleLatestJson(version, release.body ?? "", new Date().toISOString(), entries);
writeFileSync("latest.json", doc);
const upload = spawnSync("gh", ["release", "upload", tag, "latest.json", "--clobber"], {
  encoding: "utf8",
});
if (upload.status !== 0) fail(`gh release upload failed: ${upload.stderr}`);
ok(`latest.json published to ${tag}`);
