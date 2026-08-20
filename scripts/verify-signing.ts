// Conditional signing verification for release artifacts.
//   --bundle nsis  →  Authenticode status of the Windows installer
//   --bundle dmg   →  codesign/spctl/stapler on the .dmg artifact
//   --bundle dmg --phase pre-notarization → distribution signature only
//
// Policy (fail-closed where it matters):
//   * signing secrets present (APPLE_CERTIFICATE / WINDOWS_CERTIFICATE) →
//     the artifact MUST verify. Any failure aborts — a release must never
//     ship as "signed" when it silently is not.
//   * secrets absent → the build is intentionally unsigned; assert the
//     artifact is genuinely NOT notarized/signed (warn-level so a future
//     runner-side auto-sign cannot drift the state unnoticed).
//
// Pure decision/parsing logic is exported and unit-tested via --self-test,
// because the strong branches only execute on real signing runs.
//
// TAURI_SIGNING_PRIVATE_KEY (updater manifest signature) is intentionally
// out of scope here — it signs update metadata, not the installer.

import { spawnSync } from "node:child_process";
import { lstatSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { repoRoot, fail, ok, info } from "./lib/common.ts";
import { expectedSigned, parseAuthenticode, toolRan, type Env } from "./lib/signing.ts";
import {
  bundleArtifactCandidates,
  type PublicBundle,
} from "./lib/release-artifacts.ts";

function bundleArgValue(): string | undefined {
  const i = process.argv.indexOf("--bundle");
  return i >= 0 ? process.argv[i + 1] : undefined;
}

function phaseArgValue(): "final" | "pre-notarization" {
  const i = process.argv.indexOf("--phase");
  const value = i >= 0 ? process.argv[i + 1] : "final";
  if (value !== "final" && value !== "pre-notarization") {
    fail("--phase must be final or pre-notarization");
  }
  return value;
}

function findArtifact(bundle: "nsis" | "msi" | "dmg"): string {
  const candidates = bundleArtifactCandidates(repoRoot, bundle);
  if (candidates.length !== 1) {
    fail(`expected exactly one ${bundle} artifact, found: ${candidates.join(", ") || "none"}`);
  }
  return candidates[0];
}

function findMacApp(): string {
  const directory = join(repoRoot, "target", "release", "bundle", "macos");
  const candidates = readdirSync(directory)
    .filter((name) => name.endsWith(".app"))
    .map((name) => join(directory, name));
  if (candidates.length !== 1) {
    fail(`expected exactly one macOS app bundle, found: ${candidates.join(", ") || "none"}`);
  }
  const metadata = lstatSync(candidates[0]);
  if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
    fail("macOS app bundle must be a real directory, not a symlink");
  }
  return candidates[0];
}

function run(cmd: string, args: string[], mustSucceed: boolean, what: string): void {
  const res = spawnSync(cmd, args, { encoding: "utf8" });
  if (mustSucceed && res.status !== 0) {
    fail(`${what} failed: ${(res.stderr ?? res.stdout ?? "").trim()}`);
  }
}

function assertToolRan(res: { status: number | null; error?: Error }, what: string): void {
  if (!toolRan(res)) {
    fail(
      `${what} did not run (${res.error?.message ?? "unknown error"}) — ` +
        "cannot verify signing state; refusing to guess",
    );
  }
}

function verifyDmg(
  dmg: string,
  expectSigned: boolean,
  phase: "final" | "pre-notarization",
): void {
  if (expectSigned) {
    // Gatekeeper's own checks against the shipped artifact: spctl walks the
    // dmg's inner signature chain, stapler confirms the notarization ticket.
    run("codesign", ["--verify", "--strict", dmg], true, "codesign verify");
    if (phase === "pre-notarization") {
      run(
        "codesign",
        ["--verify", "--deep", "--strict", "--verbose=2", findMacApp()],
        true,
        "nested app codesign verify",
      );
      const stapler = spawnSync("xcrun", ["stapler", "validate", dmg], { encoding: "utf8" });
      assertToolRan(stapler, "stapler validate");
      if (stapler.status === 0) {
        fail("pre-notarization DMG already has a stapled ticket; refusing stale build output");
      }
      ok("macOS DMG distribution signature verified before notarization");
      return;
    }
    run("spctl", ["--assess", "--type", "open", "--context", "context:primary-signature", "-vv", dmg], true, "spctl assess");
    run("xcrun", ["stapler", "validate", dmg], true, "stapler validate");
    ok("macOS DMG signature + notarization verified");
  } else {
    if (phase !== "final") fail("pre-notarization verification requires Apple signing");
    // Unsigned build: notarization must be ABSENT. (The bundler ad-hoc signs
    // the inner .app; that is not a distribution signature and must not be
    // mistaken for one — stapler is the discriminator.)
    const res = spawnSync("xcrun", ["stapler", "validate", dmg], { encoding: "utf8" });
    assertToolRan(res, "stapler validate");
    if (res.status === 0) {
      fail("unsigned build is notarized — signing state is inconsistent");
    }
    info("unsigned build: stapler confirms not notarized (expected)");
    const ident = spawnSync("codesign", ["-dv", dmg], { encoding: "utf8" });
    const sigLine = (ident.stderr ?? "")
      .split(/\r?\n/)
      .find((l) => l.includes("Signature="));
    info(`codesign identity: ${sigLine?.trim() ?? "ad-hoc/absent"}`);
  }
}

function verifyAuthenticode(artifact: string, expectSigned: boolean): void {
  // The artifact path travels via the environment, never through command
  // string interpolation — a path containing a single quote (fork product
  // names, checkout paths) must not break or inject into the -Command.
  const cmd = "(Get-AuthenticodeSignature -LiteralPath $env:DSH_VERIFY_ARTIFACT).Status";
  const res = spawnSync("powershell", ["-NoProfile", "-Command", cmd], {
    encoding: "utf8",
    env: { ...process.env, DSH_VERIFY_ARTIFACT: artifact },
  });
  assertToolRan(res, "Authenticode check");
  const status = parseAuthenticode(res.stdout ?? "");
  if (expectSigned) {
    if (status !== "Valid") {
      fail(`expected Authenticode-signed installer, got status: ${status ?? "unknown"}`);
    }
    ok("Windows installer Authenticode signature verified");
  } else {
    if (status === "Valid") {
      fail("unsigned build unexpectedly carries an Authenticode signature — state inconsistent");
    }
    info(`unsigned build: Authenticode status ${status ?? "unknown"} (expected NotSigned)`);
  }
}

function runSelfTest(): void {
  // Decision matrix: presence of the platform secret flips the expectation.
  const envs: [Env, boolean][] = [
    [{}, false],
    [{ APPLE_CERTIFICATE: "x" }, true],
    [{ WINDOWS_CERTIFICATE: "x" }, true],
    [{ APPLE_CERTIFICATE: "" }, false],
  ];
  const want: Record<string, boolean> = {
    dmg: envs[0][1],
    "dmg+apple": envs[1][1],
    "nsis+win": envs[2][1],
    "dmg+empty": envs[3][1],
  };
  if (expectedSigned("dmg", envs[0][0]) !== want.dmg) fail("self-test: dmg no-secret decision wrong");
  if (expectedSigned("dmg", envs[1][0]) !== want["dmg+apple"]) fail("self-test: dmg apple decision wrong");
  if (expectedSigned("nsis", envs[2][0]) !== want["nsis+win"]) fail("self-test: nsis win decision wrong");
  if (expectedSigned("msi", envs[2][0]) !== want["nsis+win"]) fail("self-test: msi win decision wrong");
  if (expectedSigned("dmg", envs[3][0]) !== want["dmg+empty"]) fail("self-test: empty-secret decision wrong");

  // Parser: real Authenticode outputs (CRLF, extra whitespace, localized
  // wrappers must not break the first-line extraction).
  if (parseAuthenticode("Valid\r\n") !== "Valid") fail("self-test: Valid parse wrong");
  if (parseAuthenticode("  NotSigned\r\n") !== "NotSigned") fail("self-test: NotSigned parse wrong");
  if (parseAuthenticode("HashMismatch\n") !== "HashMismatch") fail("self-test: HashMismatch parse wrong");
  if (parseAuthenticode("\r\n") !== null) fail("self-test: empty output must parse to null");

  // Tool-availability discriminator: null status (tool missing) must never
  // count as a verification result — the unsigned branch is fail-open
  // otherwise.
  if (toolRan({ status: 0 }) !== true) fail("self-test: status 0 must count as ran");
  if (toolRan({ status: 1 }) !== true) fail("self-test: non-zero status still ran");
  if (toolRan({ status: null }) !== false) fail("self-test: null status must count as NOT ran");
  if (toolRan({ status: null, error: new Error("ENOENT") }) !== false) {
    fail("self-test: errored spawn must count as NOT ran");
  }
  ok("self-test: signing decision matrix + Authenticode parser + tool-ran discriminator");
}

if (process.argv.includes("--self-test")) {
  runSelfTest();
  process.exit(0);
}

const bundleType = bundleArgValue();
if (bundleType !== "nsis" && bundleType !== "msi" && bundleType !== "dmg") {
  fail("usage: node scripts/verify-signing.ts --bundle <nsis|msi|dmg> [--self-test]");
}

const expect = expectedSigned(bundleType, process.env);
const phase = phaseArgValue();
if (phase === "pre-notarization" && bundleType !== "dmg") {
  fail("pre-notarization phase is only valid for DMG artifacts");
}
if (bundleType === "dmg") {
  verifyDmg(findArtifact("dmg"), expect, phase);
} else {
  verifyAuthenticode(findArtifact(bundleType), expect);
}
ok(
  `signing state verified: ${bundleType} is ${expect ? "signed" : "unsigned (expected)"}`,
);
