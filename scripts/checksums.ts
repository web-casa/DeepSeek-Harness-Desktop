// Emit "<artifact>.sha256" next to a built installer so unsigned downloads
// can be verified. Mirrors the release workflow's artifact uploads.

import { readdirSync, createReadStream, writeFileSync, existsSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, basename } from "node:path";
import { repoRoot, fail, ok } from "./lib/common.ts";

const bundleArg = process.argv.indexOf("--bundle");
const bundleType = bundleArg >= 0 ? process.argv[bundleArg + 1] : undefined;
if (bundleType !== "nsis" && bundleType !== "dmg") {
  fail("usage: node scripts/checksums.ts --bundle <nsis|dmg>");
}

const bundleDir = join(repoRoot, "target", "release", "bundle", bundleType);
if (!existsSync(bundleDir)) {
  fail(`bundle dir missing at ${bundleDir} — run the bundle build first`);
}
const suffix = bundleType === "nsis" ? "-setup.exe" : ".dmg";
const artifacts = readdirSync(bundleDir).filter((f) => f.endsWith(suffix));
if (artifacts.length !== 1) {
  fail(`expected exactly one ${suffix} artifact in ${bundleDir}, found: ${artifacts.join(", ") || "none"}`);
}

const artifact = join(bundleDir, artifacts[0]);
const hash = createHash("sha256");
const stream = createReadStream(artifact);
for await (const chunk of stream) {
  hash.update(chunk);
}
const digest = hash.digest("hex");
const out = `${artifact}.sha256`;
writeFileSync(out, `${digest}  ${basename(artifact)}\n`);
ok(`${basename(artifact)} sha256 → ${digest}`);
ok(`written ${out}`);
