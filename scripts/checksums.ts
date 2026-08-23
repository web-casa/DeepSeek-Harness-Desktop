// Emit "<artifact>.sha256" next to a built installer so unsigned downloads
// can be verified. Mirrors the release workflow's artifact uploads.

import { createReadStream, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { basename } from "node:path";
import { repoRoot, fail, ok } from "./lib/common.ts";
import {
  BUNDLE_SPECS,
  bundleArtifactCandidates,
  type PublicBundle,
} from "./lib/release-artifacts.ts";
import {
  isWindowsWixInstallerLocale,
  type WindowsWixInstallerLocale,
} from "./lib/windows-installer-locales.ts";
import { sha256SidecarContent } from "./lib/release-checksums.ts";

const bundleArg = process.argv.indexOf("--bundle");
const bundleType = bundleArg >= 0 ? process.argv[bundleArg + 1] : undefined;
if (bundleType === undefined || !Object.hasOwn(BUNDLE_SPECS, bundleType)) {
  fail(
    `usage: node scripts/checksums.ts --bundle <${Object.keys(BUNDLE_SPECS).join("|")}> [--installer-locale <en-US|zh-CN>]`,
  );
}
const bundle = bundleType as PublicBundle;
const localeIndex = process.argv.indexOf("--installer-locale");
const installerLocale = localeIndex >= 0 ? process.argv[localeIndex + 1] : undefined;
if (installerLocale !== undefined && !isWindowsWixInstallerLocale(installerLocale)) {
  fail("--installer-locale must be en-US or zh-CN");
}
if ((bundle === "msi") !== (installerLocale !== undefined)) {
  fail("--installer-locale is required for MSI and forbidden for other bundles");
}
const artifacts = bundleArtifactCandidates(
  repoRoot,
  bundle,
  installerLocale as WindowsWixInstallerLocale | undefined,
);
if (artifacts.length !== 1) {
  fail(
    `expected exactly one ${bundle}${installerLocale ? ` (${installerLocale})` : ""} artifact in ${BUNDLE_SPECS[bundle].directory}, found: ${artifacts.map((path) => basename(path)).join(", ") || "none"}`,
  );
}

const artifact = artifacts[0];
const hash = createHash("sha256");
const stream = createReadStream(artifact);
for await (const chunk of stream) {
  hash.update(chunk);
}
const digest = hash.digest("hex");
const out = `${artifact}.sha256`;
writeFileSync(out, sha256SidecarContent(digest, basename(artifact)));
ok(`${basename(artifact)} sha256 → ${digest}`);
ok(`written ${out}`);
