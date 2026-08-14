// Verify a built installer's contents before release:
//   --bundle nsis  →  7z technical listing + extraction of the NSIS setup exe
//   --bundle dmg   →  hdiutil attach + walk of the .app bundle
//
// Asserts: main binary present and of the right type (PE/Mach-O), the full
// runtime tree (node/sidecar/harness entry) is bundled, the platform-specific
// node-pty prebuild survived, and the harness tree contains ZERO symlinks.
//
// Runs inside the platform build job, so 7z (Windows runner) / hdiutil
// (macOS runner) are always available.

import {
  existsSync,
  readdirSync,
  readFileSync,
  lstatSync,
  mkdirSync,
  rmSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { repoRoot, tmpDir, readManifest, fail, ok, info } from "./lib/common.ts";

const bundleArg = process.argv.indexOf("--bundle");
const bundleType = bundleArg >= 0 ? process.argv[bundleArg + 1] : undefined;
if (process.argv.includes("--self-test")) {
  runSelfTest();
  process.exit(0);
}
if (bundleType !== "nsis" && bundleType !== "dmg") {
  fail("usage: node scripts/verify-bundle.ts --bundle <nsis|dmg> [--self-test]");
}

const config = JSON.parse(
  readFileSync(join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"),
) as { productName?: string };
const productName = config.productName ?? "DeepSeek Harness Desktop";
const bundleDir = join(repoRoot, "target", "release", "bundle", bundleType);

function findArtifact(): string {
  if (!existsSync(bundleDir)) {
    fail(`bundle dir missing at ${bundleDir} — run the bundle build first`);
  }
  const suffix = bundleType === "nsis" ? "-setup.exe" : ".dmg";
  const candidates = readdirSync(bundleDir).filter((f) => f.endsWith(suffix));
  if (candidates.length !== 1) {
    fail(`expected exactly one ${suffix} artifact in ${bundleDir}, found: ${candidates.join(", ") || "none"}`);
  }
  return join(bundleDir, candidates[0]);
}

function checkBinaryType(path: string, want: "PE" | "Mach-O", label: string): void {
  const res = spawnSync("file", [path], { encoding: "utf8" });
  const out = res.stdout ?? "";
  if (res.status !== 0 || !out.includes(want)) {
    fail(`binary type check failed for ${label}: ${out.trim() || res.stderr}`);
  }
  ok(`${label} is ${want}`);
}

// ---------------------------------------------------------------------------
// NSIS: 7z technical listing (-slt) → parse "Path = " entries.
// ---------------------------------------------------------------------------
function find7z(): string {
  for (const cmd of ["7z", "7za", "7zr"]) {
    const res = spawnSync(cmd, ["-h"], { encoding: "utf8" });
    if (res.status === 0) return cmd;
  }
  for (const path of ["C:\\Program Files\\7-Zip\\7z.exe", "C:\\Program Files (x86)\\7-Zip\\7z.exe"]) {
    if (existsSync(path)) return path;
  }
  fail("7z not found on PATH or in default Windows locations");
}

function parseSltListing(text: string): string[] {
  const entries: string[] = [];
  // The -slt output opens with an archive-level block (Path = <archive path>)
  // that must NOT be treated as a content entry. Only collect paths after the
  // long dash separator that precedes the first real entry.
  let inEntries = false;
  for (const line of text.split(/\r?\n/)) {
    if (!inEntries) {
      if (/^-{10,}$/.test(line)) inEntries = true;
      continue;
    }
    if (line.startsWith("Path = ")) {
      entries.push(line.slice("Path = ".length).replace(/\\/g, "/"));
    }
  }
  return entries;
}

function listNsisEntries(artifact: string): string[] {
  const sevenZip = find7z();
  // -slt output for ~2k entries exceeds spawnSync's 1MB default buffer —
  // the child gets killed mid-listing otherwise (status null, ENOBUFS).
  const res = spawnSync(sevenZip, ["l", "-slt", artifact], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (res.status !== 0) {
    fail(
      `7z listing failed (exit ${res.status}, error ${res.error?.message ?? "none"}) for ${artifact}\n  command: ${sevenZip} l -slt <artifact>\n  stdout tail: ${(res.stdout ?? "").trim().slice(-400)}\n  stderr: ${(res.stderr ?? "").trim()}`,
    );
  }
  return parseSltListing(res.stdout ?? "");
}

// Fixture-driven check of the -slt parser, runnable on any platform:
// `node scripts/verify-bundle.ts --self-test`
function runSelfTest(): void {
  const fixture = `
7-Zip 24.09 (x64) : Copyright (c) 1999-2024 Igor Pavlov : 2024-11-29

Scanning the drive for archives:
1 file, 54822827 bytes (53 MiB)

Listing archive: installer.exe

--
Path = D:\\a\\dsh-gui\\dsh-gui\\src-tauri\\target\\release\\bundle\\nsis\\installer.exe
Type = Nsis
Physical Size = 54822827

----------
Path = DeepSeek Harness Desktop.exe
Folder = -
Size = 18937433
Packed Size = 11938431
Modified = 2026-08-14 04:22:30
Attributes = A_ -rwxr-xr-x
CRC = 3B7B40D8
Encrypted = -
Method = LZMA:26

Path = runtime\\node.exe
Size = 92119040

Path = runtime/harness/node_modules/@deepseek-ai/dsh/lib/bin.js
Size = 2001
`;
  const entries = parseSltListing(fixture);
  const want = [
    "DeepSeek Harness Desktop.exe",
    "runtime/node.exe",
    "runtime/harness/node_modules/@deepseek-ai/dsh/lib/bin.js",
  ];
  const got = new Set(entries.map((e) => e.toLowerCase()));
  for (const path of want) {
    if (!got.has(path.toLowerCase())) {
      fail(`self-test: parser missed ${path} (parsed ${entries.length} entries)`);
    }
  }
  if (entries.length !== 3) {
    fail(`self-test: expected 3 entries (header block excluded), got ${entries.length}: ${entries.join(", ")}`);
  }
  if (entries.some((e) => e.toLowerCase().endsWith("installer.exe"))) {
    fail("self-test: parser must exclude the archive-level header block");
  }
  ok("self-test: 7z -slt parser excludes header block, handles spaces and backslashes");
}

function extractNsisFile(artifact: string, innerPath: string, outDir: string): string {
  const res = spawnSync(find7z(), ["e", artifact, `-o${outDir}`, innerPath, "-y"], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (res.status !== 0) fail(`7z extraction of ${innerPath} failed: ${res.stderr}`);
  return join(outDir, innerPath.split("/").pop()!);
}

// ---------------------------------------------------------------------------
// DMG: hdiutil attach → walk → detach.
// ---------------------------------------------------------------------------
function attachDmg(artifact: string): string {
  const mount = join(tmpDir, "dmg-mount");
  rmSync(mount, { recursive: true, force: true });
  mkdirSync(mount, { recursive: true });
  const res = spawnSync(
    "hdiutil",
    ["attach", "-nobrowse", "-readonly", "-mountpoint", mount, artifact],
    { encoding: "utf8" },
  );
  if (res.status !== 0) throw new Error(`hdiutil attach failed: ${res.stderr}`);
  return mount;
}

function detachDmg(mount: string): void {
  // Never throw from here: cleanup must not mask the primary error. Retry
  // with -force once; only remove the mountpoint after a successful detach
  // (rm -r on a still-mounted read-only volume would throw and mask errors).
  let res = spawnSync("hdiutil", ["detach", mount], { encoding: "utf8" });
  if (res.status !== 0) {
    res = spawnSync("hdiutil", ["detach", "-force", mount], { encoding: "utf8" });
  }
  if (res.status === 0) {
    rmSync(mount, { recursive: true, force: true });
  } else {
    console.warn(`⚠ hdiutil detach failed for ${mount}: ${res.stderr}`);
  }
}

function walkFiles(root: string): string[] {
  const out: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(path);
      } else if (entry.isFile()) {
        out.push(path);
      }
    }
  };
  walk(root);
  return out;
}

function countSymlinks(root: string): number {
  let links = 0;
  const scan = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) {
        links += 1;
        continue;
      }
      if (stat.isDirectory()) scan(path);
    }
  };
  scan(root);
  return links;
}

// ---------------------------------------------------------------------------
// Per-bundle required files, relative to the install root (.app contents for
// dmg). Forward slashes; comparisons are case-insensitive.
// ---------------------------------------------------------------------------
const HARNESS_CORE = [
  "runtime/harness/package.json",
  "runtime/harness/runtime-manifest.json",
  "runtime/harness/node_modules/@deepseek-ai/dsh/package.json",
  "runtime/harness/node_modules/@deepseek-ai/dsh/lib/bin.js",
];

function runNsisChecks(): void {
  const artifact = findArtifact();
  const entries = listNsisEntries(artifact);
  const entrySet = new Set(entries.map((e) => e.toLowerCase()));
  info(`NSIS archive contains ${entries.length} entries`);

  const required = [
    "runtime/node.exe",
    "runtime/sidecar.exe",
    ...HARNESS_CORE,
    "runtime/harness/node_modules/node-pty/prebuilds/win32-x64/pty.node",
  ];
  for (const path of required) {
    if (!entrySet.has(path.toLowerCase())) {
      throw new Error(`NSIS missing required entry: ${path}`);
    }
    ok(`present: ${path}`);
  }

  // Main GUI binary: exactly one top-level .exe (tauri names it after the
  // crate name; uninstall.exe lives alongside it — exclude uninstallers).
  const topLevelExes = entries.filter(
    (p) =>
      !p.includes("/") &&
      p.toLowerCase().endsWith(".exe") &&
      !p.toLowerCase().startsWith("uninst"),
  );
  if (topLevelExes.length !== 1) {
    throw new Error(
      `expected exactly 1 top-level app .exe in NSIS, found: ${topLevelExes.join(", ") || "none"}`,
    );
  }
  ok(`main binary entry: ${topLevelExes[0]}`);

  const extractDir = join(tmpDir, "nsis-extract");
  rmSync(extractDir, { recursive: true, force: true });
  mkdirSync(extractDir, { recursive: true });
  try {
    const mainExe = extractNsisFile(artifact, topLevelExes[0], extractDir);
    checkBinaryType(mainExe, "PE", "main binary");
    const manifestFile = extractNsisFile(
      artifact,
      "runtime/harness/runtime-manifest.json",
      extractDir,
    );
    checkBundledManifest(readFileSync(manifestFile, "utf8"));
  } finally {
    rmSync(extractDir, { recursive: true, force: true });
  }
}

function runDmgChecks(): void {
  const artifact = findArtifact();
  // Attach is covered by the same try/finally as everything after it: any
  // throw post-attach (missing .app, walk failure, assertion) must still
  // detach the volume — a leaked read-only mount would fail CI cleanup.
  const mount = attachDmg(artifact);
  try {
    const appRoot = join(mount, `${productName}.app`);
    if (!existsSync(appRoot)) {
      const listing = readdirSync(mount).join(", ");
      throw new Error(`.app bundle not found in DMG (entries: ${listing})`);
    }
    const allFiles = walkFiles(appRoot);
    const rel = allFiles.map((f) => f.slice(appRoot.length + 1).replace(/\\/g, "/"));
    const relSet = new Set(rel.map((f) => f.toLowerCase()));
    info(`app bundle contains ${rel.length} files`);

    const required = [
      "Contents/MacOS/deepseek-harness-desktop",
      "Contents/Resources/runtime/node",
      "Contents/Resources/runtime/sidecar",
      ...HARNESS_CORE.map((p) => `Contents/Resources/${p}`),
      "Contents/Resources/runtime/harness/node_modules/node-pty/prebuilds/darwin-arm64/pty.node",
    ];
    for (const path of required) {
      if (!relSet.has(path.toLowerCase())) {
        throw new Error(`DMG missing required file: ${path}`);
      }
      ok(`present: ${path}`);
    }

    // The whole harness subtree (licenses/, manifest, READMEs, node_modules)
    // must be materialized — any symlink breaks the self-contained bundle.
    const links = countSymlinks(
      join(appRoot, "Contents", "Resources", "runtime", "harness"),
    );
    if (links > 0) {
      throw new Error(`bundled harness tree contains ${links} symlink(s)`);
    }
    ok("bundled harness tree is fully materialized (no symlinks)");

    checkBinaryType(
      join(appRoot, "Contents", "MacOS", "deepseek-harness-desktop"),
      "Mach-O",
      "main binary",
    );

    checkBundledManifest(
      readFileSync(
        join(appRoot, "Contents", "Resources", "runtime", "harness", "runtime-manifest.json"),
        "utf8",
      ),
    );
  } finally {
    detachDmg(mount);
  }
}

/// The bundle's runtime-manifest.json must match the repo manifest: the
/// installer is verified against what the SOURCE says it should contain,
/// not just against its own internal consistency.
function checkBundledManifest(bundledText: string): void {
  const repo = readManifest();
  const bundled = JSON.parse(bundledText) as Partial<Record<string, string>>;
  for (const key of ["desktopVersion", "nodeVersion", "harnessVersion", "sidecarVersion"] as const) {
    if (bundled[key] !== repo[key]) {
      throw new Error(
        `bundled manifest ${key}=${bundled[key]} != repo manifest ${repo[key]}`,
      );
    }
  }
  ok(`bundled manifest matches repo (desktop ${bundled.desktopVersion}, node ${bundled.nodeVersion}, harness ${bundled.harnessVersion})`);
}

try {
  if (bundleType === "nsis") {
    runNsisChecks();
  } else {
    runDmgChecks();
  }
} catch (e) {
  console.error(`\n✗ ${(e as Error).message}`);
  process.exit(1);
}
ok(`${bundleType} bundle verification passed`);
