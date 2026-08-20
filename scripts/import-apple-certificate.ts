// Import the Developer ID PKCS#12 into a job-scoped keychain so both this
// repository's nested-runtime signer and Tauri use the SAME identity.
//
// Tauri normally creates its own private keychain from APPLE_CERTIFICATE, but
// that keychain only exists inside the bundler process.  The bundled Harness
// is an arbitrary Resources tree, so Tauri does not discover its Mach-O files
// as nested code.  We need the identity earlier to sign those files inside-out.

import {
  appendFileSync,
  chmodSync,
  existsSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { randomBytes } from "node:crypto";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fail, ok } from "./lib/common.ts";
import { parseCodesignIdentities, parseKeychainList } from "./lib/macos-signing.ts";

if (process.platform !== "darwin") fail("Apple certificate import must run on macOS");

const encoded = process.env.APPLE_CERTIFICATE ?? "";
const certificatePassword = process.env.APPLE_CERTIFICATE_PASSWORD ?? "";
const githubEnv = process.env.GITHUB_ENV ?? "";
const runnerTemp = process.env.RUNNER_TEMP ?? "";
if (!encoded || !certificatePassword || !githubEnv || !runnerTemp) {
  fail("APPLE_CERTIFICATE, APPLE_CERTIFICATE_PASSWORD, GITHUB_ENV and RUNNER_TEMP are required");
}

function security(args: string[], what: string): string {
  const result = spawnSync("security", args, { encoding: "utf8" });
  if (result.status !== 0 || result.error) {
    const diagnostic = [result.stdout, result.stderr].filter(Boolean).join("\n").trim();
    // Throw instead of calling common.fail(): process.exit() would bypass the
    // keychain/P12 cleanup in the surrounding catch/finally.
    throw new Error(`${what} failed${diagnostic ? `: ${diagnostic}` : ""}`);
  }
  return result.stdout ?? "";
}

const compact = encoded.replace(/\s/g, "");
if (
  compact.length === 0 ||
  compact.length % 4 !== 0 ||
  !/^[A-Za-z0-9+/]+={0,2}$/.test(compact)
) {
  fail("APPLE_CERTIFICATE is not valid canonical base64");
}

const certificate = Buffer.from(compact, "base64");
if (certificate.byteLength < 512) fail("decoded APPLE_CERTIFICATE is unexpectedly small");

const nonce = randomBytes(12).toString("hex");
const keychainPassword = randomBytes(24).toString("hex");
const certificatePath = join(runnerTemp, `dsh-signing-${nonce}.p12`);
const keychainPath = join(runnerTemp, `dsh-signing-${nonce}.keychain-db`);
let keychainCreated = false;

try {
  writeFileSync(certificatePath, certificate, { mode: 0o600, flag: "wx" });
  chmodSync(certificatePath, 0o600);

  const existingKeychains = parseKeychainList(
    security(["list-keychains", "-d", "user"], "list existing keychains"),
  );
  security(["create-keychain", "-p", keychainPassword, keychainPath], "create signing keychain");
  keychainCreated = true;
  security(["set-keychain-settings", "-lut", "21600", keychainPath], "configure signing keychain");
  security(["unlock-keychain", "-p", keychainPassword, keychainPath], "unlock signing keychain");
  security(
    [
      "import",
      certificatePath,
      "-P",
      certificatePassword,
      "-T",
      "/usr/bin/codesign",
      "-T",
      "/usr/bin/pkgbuild",
      "-T",
      "/usr/bin/productbuild",
      "-t",
      "cert",
      "-f",
      "pkcs12",
      "-k",
      keychainPath,
    ],
    "import Developer ID certificate",
  );
  security(
    [
      "set-key-partition-list",
      "-S",
      "apple-tool:,apple:,codesign:",
      "-s",
      "-k",
      keychainPassword,
      keychainPath,
    ],
    "authorize codesign key access",
  );
  security(
    ["list-keychains", "-d", "user", "-s", ...existingKeychains, keychainPath],
    "add signing keychain to search list",
  );

  const identities = parseCodesignIdentities(
    security(
      ["find-identity", "-v", "-p", "codesigning", keychainPath],
      "resolve signing identity",
    ),
  ).filter((identity) => identity.startsWith("Developer ID Application:"));
  if (identities.length !== 1) {
    throw new Error(
      `expected exactly one Developer ID Application identity, found ${identities.length}`,
    );
  }

  const identity = identities[0];
  if (/[\r\n]/.test(identity) || /[\r\n]/.test(keychainPath)) {
    throw new Error("refusing unsafe newline in signing environment output");
  }
  appendFileSync(
    githubEnv,
    `APPLE_SIGNING_IDENTITY=${identity}\nAPPLE_KEYCHAIN_PATH=${keychainPath}\n`,
  );
  ok(`job-scoped Developer ID identity imported: ${identity}`);
} catch (error) {
  if (keychainCreated && existsSync(keychainPath)) {
    spawnSync("security", ["delete-keychain", keychainPath], { encoding: "utf8" });
  }
  throw error;
} finally {
  rmSync(certificatePath, { force: true });
}
