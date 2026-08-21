// Fail closed before publishing: prove that every updater signature in the
// public inventory authenticates its adjacent artifact under the exact key
// embedded in the desktop application.

import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readFileSync,
  readdirSync,
} from "node:fs";
import { basename, join } from "node:path";
import { fail, info, ok } from "./lib/common.ts";
import { expectedUpdaterSignatureCount } from "./lib/release-inventory.ts";
import { verifyTauriUpdaterSignature } from "./lib/minisign.ts";

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
      if (stat.isSymbolicLink()) fail(`updater inventory contains a symlink: ${path}`);
      if (stat.isDirectory()) visit(path);
      else if (stat.isFile()) files.push(path);
      else fail(`updater inventory contains a non-regular file: ${path}`);
    }
  };
  visit(root);
  return files;
}

function readRegularFile(path: string): Buffer {
  let descriptor: number;
  try {
    descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  } catch (error) {
    fail(`cannot securely open updater file ${path}: ${String(error)}`);
  }
  try {
    if (!fstatSync(descriptor).isFile()) {
      fail(`updater path is not a regular file: ${path}`);
    }
    return readFileSync(descriptor);
  } finally {
    closeSync(descriptor);
  }
}

const directory = argument("--directory");
if (!directory) fail("usage: node scripts/verify-updater-signatures.ts --directory <root>");

const config = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8")) as {
  plugins?: { updater?: { pubkey?: unknown } };
};
const publicKey = config.plugins?.updater?.pubkey;
if (typeof publicKey !== "string" || publicKey.length === 0) {
  fail("tauri.conf.json has no updater public key");
}

const signatures = walk(directory).filter((path) => path.endsWith(".sig"));
const expected = expectedUpdaterSignatureCount();
if (signatures.length !== expected) {
  fail(`updater signature count ${signatures.length} != expected ${expected}`);
}

for (const signaturePath of signatures) {
  const artifactPath = signaturePath.slice(0, -".sig".length);
  try {
    verifyTauriUpdaterSignature(
      readRegularFile(artifactPath),
      readRegularFile(signaturePath).toString("utf8"),
      publicKey,
    );
  } catch (error) {
    fail(`${basename(signaturePath)}: ${error instanceof Error ? error.message : String(error)}`);
  }
  ok(`updater signature verified: ${basename(artifactPath)}`);
}

info(`cryptographically verified ${signatures.length} updater artifact/signature pair(s)`);
