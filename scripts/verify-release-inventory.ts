// Fail closed before a draft GitHub Release is created: require the complete
// multi-platform installer set, one valid SHA-256 sidecar per installer, the
// expected updater signatures, and no Store MSIX or unknown file types.

import { createHash } from "node:crypto";
import { createReadStream, lstatSync, readdirSync, readFileSync } from "node:fs";
import { basename, join } from "node:path";
import { fail, ok, info } from "./lib/common.ts";
import {
  classifyPublicInstaller,
  expectedPublicBundleCounts,
  msiLocaleInventoryProblems,
  expectedUpdaterSignatureCount,
} from "./lib/release-inventory.ts";
import type { PublicBundle } from "./lib/release-artifacts.ts";

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function walk(root: string): string[] {
  const files: string[] = [];
  const visit = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) fail(`release inventory contains a symlink: ${path}`);
      if (stat.isDirectory()) visit(path);
      else if (stat.isFile()) files.push(path);
      else fail(`release inventory contains a non-regular file: ${path}`);
    }
  };
  visit(root);
  return files;
}

async function sha256(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

const directory = argument("--directory");
if (!directory) fail("usage: node scripts/verify-release-inventory.ts --directory <artifact-root>");
const files = walk(directory);
const byBasename = new Map<string, string>();
for (const path of files) {
  const name = basename(path);
  if (byBasename.has(name)) fail(`duplicate GitHub Release asset basename: ${name}`);
  byBasename.set(name, path);
}
if ([...byBasename.keys()].some((name) => name.toLowerCase().endsWith(".msix"))) {
  fail("Store MSIX must never enter the public GitHub Release inventory");
}

const installers = files
  .map((path) => ({ path, bundle: classifyPublicInstaller(basename(path)) }))
  .filter((entry): entry is { path: string; bundle: PublicBundle } => entry.bundle !== null);
const counts = Object.fromEntries(
  Object.keys(expectedPublicBundleCounts()).map((bundle) => [bundle, 0]),
) as Record<PublicBundle, number>;
for (const installer of installers) counts[installer.bundle] += 1;
for (const [bundle, expected] of Object.entries(expectedPublicBundleCounts()) as [
  PublicBundle,
  number,
][]) {
  if (counts[bundle] !== expected) {
    fail(`release inventory ${bundle} count ${counts[bundle]} != expected ${expected}`);
  }
}

const msiProblems = msiLocaleInventoryProblems(
  installers
    .filter((installer) => installer.bundle === "msi")
    .map((installer) => basename(installer.path)),
);
if (msiProblems.length > 0) fail(msiProblems.join("\n"));

for (const installer of installers) {
  const name = basename(installer.path);
  const sidecarName = `${name}.sha256`;
  const sidecar = byBasename.get(sidecarName);
  if (!sidecar) fail(`missing SHA-256 sidecar for ${name}`);
  const content = readFileSync(sidecar, "utf8");
  const match = /^([a-f0-9]{64})  ([^\r\n]+)\r?\n$/.exec(content);
  if (!match || match[2] !== name) fail(`malformed SHA-256 sidecar: ${sidecarName}`);
  const actual = await sha256(installer.path);
  if (actual !== match[1]) fail(`SHA-256 mismatch for ${name}: ${match[1]} != ${actual}`);
  ok(`release checksum verified: ${name}`);
}

const signatures = files.filter((path) => path.endsWith(".sig"));
const expectedSignatures = expectedUpdaterSignatureCount();
if (signatures.length !== expectedSignatures) {
  fail(`updater signature count ${signatures.length} != expected ${expectedSignatures}`);
}
const allowed = new Set([
  ...installers.map((entry) => entry.path),
  ...installers.map((entry) => `${entry.path}.sha256`),
  ...signatures,
]);
const unknown = files.filter((path) => !allowed.has(path));
if (unknown.length > 0) {
  fail(
    `unknown files in public release inventory: ${unknown.map((path) => basename(path)).join(", ")}`,
  );
}
info(`verified ${installers.length} installers and ${signatures.length} updater signatures`);
ok("public GitHub Release inventory is complete and Store-separated");
