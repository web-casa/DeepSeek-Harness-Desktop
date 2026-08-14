// Conditional signing verification for release artifacts.
//   --bundle nsis  →  Authenticode status of the Windows installer
//   --bundle dmg   →  codesign/spctl/stapler on the macOS .app bundle
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
import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { repoRoot, fail, ok, info } from "./lib/common.ts";

export type Env = Record<string, string | undefined>;

export function expectedSigned(bundleType: string, env: Env): boolean {
  return bundleType === "dmg"
    ? Boolean(env.APPLE_CERTIFICATE)
    : Boolean(env.WINDOWS_CERTIFICATE);
}

/// First non-empty trimmed line of `(Get-AuthenticodeSignature …).Status`:
/// "Valid" / "NotSigned" / "HashMismatch" / "NotTrusted" / "UnknownError".
export function parseAuthenticode(stdout: string): string | null {
  const first = stdout
    .split(/\r?\n/)
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  return first ?? null;
}

function bundleArgValue(): string | undefined {
  const i = process.argv.indexOf("--bundle");
  return i >= 0 ? process.argv[i + 1] : undefined;
}

function findAppBundle(): string {
  const macosDir = join(repoRoot, "target", "release", "bundle", "macos");
  if (!existsSync(macosDir)) {
    fail("macos bundle dir missing — run the bundle build first");
  }
  const candidates = readdirSync(macosDir).filter((f) => f.endsWith(".app"));
  if (candidates.length !== 1) {
    fail(`expected exactly one .app in ${macosDir}, found: ${candidates.join(", ") || "none"}`);
  }
  return join(macosDir, candidates[0]);
}

function findNsisArtifact(): string {
  const bundleDir = join(repoRoot, "target", "release", "bundle", "nsis");
  if (!existsSync(bundleDir)) {
    fail("nsis bundle dir missing — run the bundle build first");
  }
  const candidates = readdirSync(bundleDir).filter((f) => f.endsWith("-setup.exe"));
  if (candidates.length !== 1) {
    fail(`expected exactly one setup exe in ${bundleDir}, found: ${candidates.join(", ") || "none"}`);
  }
  return join(bundleDir, candidates[0]);
}

function run(cmd: string, args: string[], mustSucceed: boolean, what: string): void {
  const res = spawnSync(cmd, args, { encoding: "utf8" });
  if (mustSucceed && res.status !== 0) {
    fail(`${what} failed: ${(res.stderr ?? res.stdout ?? "").trim()}`);
  }
}

function verifyDmg(app: string, expectSigned: boolean): void {
  if (expectSigned) {
    run("codesign", ["--verify", "--deep", "--strict", app], true, "codesign verify");
    run("spctl", ["--assess", "--type", "execute", "-vv", app], true, "spctl assess");
    run("stapler", ["validate", app], true, "stapler validate");
    ok("macOS bundle signature + notarization verified");
  } else {
    // Unsigned build: notarization must be ABSENT. (The bundler ad-hoc signs
    // the .app; that is not a distribution signature and must not be
    // mistaken for one — stapler is the discriminator.)
    const res = spawnSync("stapler", ["validate", app], { encoding: "utf8" });
    if (res.status === 0) {
      fail("unsigned build is notarized — signing state is inconsistent");
    }
    info("unsigned build: stapler confirms not notarized (expected)");
    const ident = spawnSync("codesign", ["-dv", app], { encoding: "utf8" });
    const sigLine = (ident.stderr ?? "")
      .split(/\r?\n/)
      .find((l) => l.includes("Signature="));
    info(`codesign identity: ${sigLine?.trim() ?? "ad-hoc/absent"}`);
  }
}

function verifyNsis(artifact: string, expectSigned: boolean): void {
  const cmd = `(Get-AuthenticodeSignature -LiteralPath '${artifact}').Status`;
  const res = spawnSync("powershell", ["-NoProfile", "-Command", cmd], {
    encoding: "utf8",
  });
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
  if (expectedSigned("dmg", envs[3][0]) !== want["dmg+empty"]) fail("self-test: empty-secret decision wrong");

  // Parser: real Authenticode outputs (CRLF, extra whitespace, localized
  // wrappers must not break the first-line extraction).
  if (parseAuthenticode("Valid\r\n") !== "Valid") fail("self-test: Valid parse wrong");
  if (parseAuthenticode("  NotSigned\r\n") !== "NotSigned") fail("self-test: NotSigned parse wrong");
  if (parseAuthenticode("HashMismatch\n") !== "HashMismatch") fail("self-test: HashMismatch parse wrong");
  if (parseAuthenticode("\r\n") !== null) fail("self-test: empty output must parse to null");
  ok("self-test: signing decision matrix + Authenticode parser");
}

if (process.argv.includes("--self-test")) {
  runSelfTest();
  process.exit(0);
}

const bundleType = bundleArgValue();
if (bundleType !== "nsis" && bundleType !== "dmg") {
  fail("usage: node scripts/verify-signing.ts --bundle <nsis|dmg> [--self-test]");
}

const expect = expectedSigned(bundleType, process.env);
if (bundleType === "dmg") {
  verifyDmg(findAppBundle(), expect);
} else {
  verifyNsis(findNsisArtifact(), expect);
}
ok(
  `signing state verified: ${bundleType} is ${expect ? "signed" : "unsigned (expected)"}`,
);
