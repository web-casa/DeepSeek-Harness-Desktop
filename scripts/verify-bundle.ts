// Verify every public installer before release.
//
// Each package is expanded with its platform-native tool, then checked against
// one runtime contract: correct host architecture, executable main/Node/
// sidecar binaries, exact runtime manifest, selected node-pty prebuild,
// dsharness deep-link registration, and a fully materialized Harness tree.

import {
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { spawnSync, type SpawnSyncOptionsWithStringEncoding } from "node:child_process";
import { basename, dirname, join, relative } from "node:path";
import { repoRoot, tmpDir, readManifest, fail, ok, info } from "./lib/common.ts";
import { quarantinePresent, parseSltListing } from "./lib/bundle-checks.ts";
import {
  BUNDLE_SPECS,
  bundleArtifactCandidates,
  type PublicBundle,
  type ReleaseArch,
} from "./lib/release-artifacts.ts";
import { flatpakMetadataProblems } from "./lib/flatpak.ts";

type RuntimePlatform = "win32" | "darwin" | "linux";
type BinaryKind = "PE" | "Mach-O" | "ELF";

const HARNESS_CORE = [
  "harness/package.json",
  "harness/runtime-manifest.json",
  "harness/node_modules/@deepseek-ai/dsh/package.json",
  "harness/node_modules/@deepseek-ai/dsh/lib/bin.js",
  "harness/node_modules/pnpm/bin/pnpm.cjs",
  "harness/licenses/@deepseek-ai/dsh/LICENSE",
] as const;

const config = JSON.parse(
  readFileSync(join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"),
) as {
  productName?: string;
  identifier?: string;
  plugins?: { "deep-link"?: { desktop?: { schemes?: string[] } } };
};
const productName = config.productName ?? "DSH Desktop";
const appIdentifier = config.identifier ?? "com.yeagoo.dsh-desktop";

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function parseBundle(value: string | undefined): PublicBundle {
  if (value !== undefined && Object.hasOwn(BUNDLE_SPECS, value)) {
    return value as PublicBundle;
  }
  fail(
    `usage: node scripts/verify-bundle.ts --bundle <${Object.keys(BUNDLE_SPECS).join("|")}> --arch <x64|arm64> [--self-test]`,
  );
}

function parseArch(value: string | undefined): ReleaseArch {
  const resolved = value ?? process.arch;
  if (resolved !== "x64" && resolved !== "arm64") {
    fail(`--arch must be x64 or arm64, got ${resolved}`);
  }
  return resolved;
}

function artifactFor(bundle: PublicBundle): string {
  const candidates = bundleArtifactCandidates(repoRoot, bundle);
  if (candidates.length !== 1) {
    const spec = BUNDLE_SPECS[bundle];
    fail(
      `expected exactly one ${spec.suffix} artifact in ${spec.directory}, found: ${candidates.map((path) => basename(path)).join(", ") || "none"}`,
    );
  }
  return candidates[0];
}

function run(
  command: string,
  args: string[],
  what: string,
  options: Partial<SpawnSyncOptionsWithStringEncoding> = {},
): string {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    ...options,
  });
  if (result.status !== 0) {
    throw new Error(
      `${what} failed (exit ${result.status}, ${result.error?.message ?? "no spawn error"}):\n${(result.stderr ?? result.stdout ?? "").trim().slice(-4000)}`,
    );
  }
  return result.stdout ?? "";
}

function checkBinaryType(
  path: string,
  kind: BinaryKind,
  arch: ReleaseArch,
  label: string,
): void {
  const output = run("file", [path], `${label} file inspection`).toLowerCase();
  const kindMarker = kind.toLowerCase();
  const archMarkers: Readonly<Record<BinaryKind, Record<ReleaseArch, readonly string[]>>> = {
    PE: { x64: ["x86-64", "x86_64"], arm64: ["aarch64", "arm64"] },
    "Mach-O": { x64: ["x86_64", "x86-64"], arm64: ["arm64"] },
    ELF: { x64: ["x86-64", "x86_64"], arm64: ["arm aarch64", "aarch64"] },
  };
  if (
    !output.includes(kindMarker) ||
    !archMarkers[kind][arch].some((marker) => output.includes(marker))
  ) {
    throw new Error(
      `binary type check failed for ${label}: expected ${kind} ${arch}, got ${output.trim()}`,
    );
  }
  ok(`${label} is ${kind} ${arch}`);
}

function assertExecutable(path: string, label: string): void {
  const mode = lstatSync(path).mode;
  if ((mode & 0o111) === 0) {
    throw new Error(`${label} is not executable (mode ${(mode & 0o777).toString(8)})`);
  }
  ok(`${label} is executable`);
}

function countSymlinks(root: string): number {
  let links = 0;
  const scan = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) {
        links += 1;
      } else if (stat.isDirectory()) {
        scan(path);
      }
    }
  };
  scan(root);
  return links;
}

function walkFiles(root: string): string[] {
  const files: string[] = [];
  const walk = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile()) files.push(path);
    }
  };
  walk(root);
  return files;
}

function assertFile(path: string, label: string): void {
  if (!existsSync(path) || !lstatSync(path).isFile()) {
    throw new Error(`${label} is missing or is not a regular file: ${path}`);
  }
  ok(`present: ${label}`);
}

function checkBundledManifest(path: string): void {
  const source = readManifest();
  const bundled = JSON.parse(readFileSync(path, "utf8")) as Partial<
    Record<"desktopVersion" | "nodeVersion" | "harnessVersion" | "sidecarVersion", string>
  >;
  for (const key of [
    "desktopVersion",
    "nodeVersion",
    "harnessVersion",
    "sidecarVersion",
  ] as const) {
    if (bundled[key] !== source[key]) {
      throw new Error(`bundled manifest ${key}=${bundled[key]} != repo manifest ${source[key]}`);
    }
  }
  ok(
    `bundled manifest matches repo (desktop ${bundled.desktopVersion}, node ${bundled.nodeVersion}, harness ${bundled.harnessVersion})`,
  );
}

function verifyRuntimeTree(
  mainBinary: string,
  runtimeRoot: string,
  platform: RuntimePlatform,
  arch: ReleaseArch,
): void {
  const extension = platform === "win32" ? ".exe" : "";
  const ptyBinary = platform === "win32" ? "conpty.node" : "pty.node";
  const required = [
    `node${extension}`,
    `sidecar${extension}`,
    ...HARNESS_CORE,
    `harness/node_modules/node-pty/prebuilds/${platform}-${arch}/${ptyBinary}`,
  ];
  assertFile(mainBinary, "main binary");
  for (const entry of required) assertFile(join(runtimeRoot, entry), entry);

  const harnessRoot = join(runtimeRoot, "harness");
  const symlinks = countSymlinks(harnessRoot);
  if (symlinks !== 0) {
    throw new Error(`bundled harness tree contains ${symlinks} symlink(s)`);
  }
  ok("bundled harness tree is fully materialized (no symlinks)");

  const kind: BinaryKind =
    platform === "win32" ? "PE" : platform === "darwin" ? "Mach-O" : "ELF";
  const node = join(runtimeRoot, `node${extension}`);
  const sidecar = join(runtimeRoot, `sidecar${extension}`);
  for (const [path, label] of [
    [mainBinary, "main binary"],
    [node, "bundled node"],
    [sidecar, "sidecar binary"],
  ] as const) {
    if (platform !== "win32") assertExecutable(path, label);
    checkBinaryType(path, kind, arch, label);
  }
  checkBundledManifest(join(runtimeRoot, "harness", "runtime-manifest.json"));
}

function assertLinuxDeepLink(root: string): void {
  const desktopFiles = walkFiles(root).filter((path) => path.endsWith(".desktop"));
  const registered = desktopFiles.find((path) =>
    /(?:^|;)x-scheme-handler\/dsharness(?:;|$)/m.test(
      (readFileSync(path, "utf8").match(/^MimeType=(.*)$/m)?.[1] ?? "").trim(),
    ),
  );
  if (!registered) {
    throw new Error(
      `no desktop entry registers x-scheme-handler/dsharness under ${root} (${desktopFiles.length} .desktop files inspected)`,
    );
  }
  ok(`Linux desktop entry registers dsharness: ${relative(root, registered)}`);
}

function verifyLinuxLayout(root: string, prefix: "usr" | "files", arch: ReleaseArch): void {
  const mainBinary = join(root, prefix, "bin", "deepseek-harness-desktop");
  const runtimeRoot = join(root, prefix, "lib", productName, "runtime");
  verifyRuntimeTree(mainBinary, runtimeRoot, "linux", arch);
  assertLinuxDeepLink(root);
}

function find7z(): string {
  for (const command of ["7z", "7za", "7zr"]) {
    const result = spawnSync(command, ["-h"], { encoding: "utf8" });
    if (result.status === 0) return command;
  }
  for (const path of [
    "C:\\Program Files\\7-Zip\\7z.exe",
    "C:\\Program Files (x86)\\7-Zip\\7z.exe",
  ]) {
    if (existsSync(path)) return path;
  }
  fail("7z not found on PATH or in default Windows locations");
}

function listNsisEntries(artifact: string): string[] {
  return parseSltListing(run(find7z(), ["l", "-slt", artifact], "7z NSIS listing"));
}

function extractNsisFile(artifact: string, innerPath: string, output: string): string {
  run(
    find7z(),
    ["e", artifact, `-o${output}`, innerPath, "-y"],
    `7z extraction of ${innerPath}`,
  );
  return join(output, basename(innerPath));
}

function verifyNsis(artifact: string, arch: ReleaseArch): void {
  const entries = listNsisEntries(artifact);
  const entrySet = new Set(entries.map((entry) => entry.toLowerCase()));
  const required = [
    "runtime/node.exe",
    "runtime/sidecar.exe",
    ...HARNESS_CORE.map((entry) => `runtime/${entry}`),
    `runtime/harness/node_modules/node-pty/prebuilds/win32-${arch}/conpty.node`,
  ];
  for (const path of required) {
    if (!entrySet.has(path.toLowerCase())) throw new Error(`NSIS missing required entry: ${path}`);
    ok(`present: ${path}`);
  }
  const mainEntries = entries.filter(
    (path) =>
      !path.includes("/") &&
      path.toLowerCase().endsWith(".exe") &&
      !path.toLowerCase().startsWith("uninst"),
  );
  if (mainEntries.length !== 1) {
    throw new Error(`expected one NSIS main executable, found ${mainEntries.join(", ") || "none"}`);
  }
  const extraction = join(tmpDir, `nsis-extract-${process.pid}`);
  rmSync(extraction, { recursive: true, force: true });
  mkdirSync(extraction, { recursive: true });
  try {
    for (const [path, label] of [
      [mainEntries[0], "main binary"],
      ["runtime/node.exe", "bundled node"],
      ["runtime/sidecar.exe", "sidecar binary"],
    ] as const) {
      checkBinaryType(extractNsisFile(artifact, path, extraction), "PE", arch, label);
    }
    const manifest = extractNsisFile(
      artifact,
      "runtime/harness/runtime-manifest.json",
      extraction,
    );
    checkBundledManifest(manifest);
  } finally {
    rmSync(extraction, { recursive: true, force: true });
  }
}

function findUniqueFile(root: string, name: string, label: string): string {
  const candidates = walkFiles(root).filter(
    (path) => basename(path).toLowerCase() === name.toLowerCase(),
  );
  if (candidates.length !== 1) {
    throw new Error(
      `expected one ${label} named ${name} under ${root}, found ${candidates.map((path) => relative(root, path)).join(", ") || "none"}`,
    );
  }
  return candidates[0];
}

function findUniqueRuntimeRoot(root: string, extension: string): string {
  const candidates = walkFiles(root)
    .filter((path) => basename(path).toLowerCase() === `node${extension}`)
    .map(dirname)
    .filter((path) => existsSync(join(path, "harness", "runtime-manifest.json")));
  if (candidates.length !== 1) {
    throw new Error(
      `expected one bundled runtime under ${root}, found ${candidates.map((path) => relative(root, path)).join(", ") || "none"}`,
    );
  }
  return candidates[0];
}

function verifyMsi(artifact: string, arch: ReleaseArch): void {
  const extraction = join(tmpDir, `msi-admin-${process.pid}`);
  const log = join(tmpDir, `msi-admin-${process.pid}.log`);
  rmSync(extraction, { recursive: true, force: true });
  rmSync(log, { force: true });
  mkdirSync(extraction, { recursive: true });
  try {
    run(
      "msiexec",
      ["/a", artifact, "/qn", `TARGETDIR=${extraction}`, "/l*v", log],
      "MSI administrative extraction",
    );
    const main = findUniqueFile(extraction, "deepseek-harness-desktop.exe", "MSI main executable");
    const runtime = findUniqueRuntimeRoot(extraction, ".exe");
    verifyRuntimeTree(main, runtime, "win32", arch);
  } finally {
    rmSync(extraction, { recursive: true, force: true });
    rmSync(log, { force: true });
  }
}

function attachDmg(artifact: string): string {
  const mount = join(tmpDir, `dmg-mount-${process.pid}`);
  rmSync(mount, { recursive: true, force: true });
  mkdirSync(mount, { recursive: true });
  run(
    "hdiutil",
    ["attach", "-nobrowse", "-readonly", "-mountpoint", mount, artifact],
    "DMG attach",
  );
  return mount;
}

function detachDmg(mount: string): void {
  let result = spawnSync("hdiutil", ["detach", mount], { encoding: "utf8" });
  if (result.status !== 0) {
    result = spawnSync("hdiutil", ["detach", "-force", mount], { encoding: "utf8" });
  }
  if (result.status === 0) rmSync(mount, { recursive: true, force: true });
  else console.warn(`⚠ hdiutil detach failed for ${mount}: ${result.stderr}`);
}

function assertDmgDeepLink(appRoot: string): void {
  const output = run(
    "plutil",
    ["-p", join(appRoot, "Contents", "Info.plist")],
    "Info.plist inspection",
  );
  if (!output.includes("CFBundleURLSchemes") || !output.includes('"dsharness"')) {
    throw new Error("Info.plist does not register the dsharness URL scheme");
  }
  ok("Info.plist registers the dsharness URL scheme");
}

function assertNoQuarantine(path: string): void {
  const result = spawnSync("xattr", ["-p", "com.apple.quarantine", path], {
    encoding: "utf8",
  });
  const present = quarantinePresent(result.status, result.error);
  if (present === null) throw new Error("xattr unavailable; cannot verify quarantine state");
  if (present) throw new Error(`app bundle carries com.apple.quarantine: ${result.stdout}`);
  ok("app bundle has no quarantine attribute");
}

function verifyDmg(artifact: string, arch: ReleaseArch): void {
  const mount = attachDmg(artifact);
  try {
    const appRoot = join(mount, `${productName}.app`);
    if (!existsSync(appRoot)) throw new Error(`${productName}.app is missing from DMG`);
    verifyRuntimeTree(
      join(appRoot, "Contents", "MacOS", "deepseek-harness-desktop"),
      join(appRoot, "Contents", "Resources", "runtime"),
      "darwin",
      arch,
    );
    assertDmgDeepLink(appRoot);
    assertNoQuarantine(appRoot);
  } finally {
    detachDmg(mount);
  }
}

function verifyDeb(artifact: string, arch: ReleaseArch): void {
  const packageName = run("dpkg-deb", ["-f", artifact, "Package"], "DEB package query").trim();
  const packageArch = run("dpkg-deb", ["-f", artifact, "Architecture"], "DEB arch query").trim();
  if (packageName !== "dsh-desktop") throw new Error(`unexpected DEB package name: ${packageName}`);
  const expectedArch = arch === "x64" ? "amd64" : "arm64";
  if (packageArch !== expectedArch) {
    throw new Error(`DEB architecture ${packageArch} != expected ${expectedArch}`);
  }
  const extraction = join(tmpDir, `deb-extract-${process.pid}`);
  rmSync(extraction, { recursive: true, force: true });
  mkdirSync(extraction, { recursive: true });
  try {
    run("dpkg-deb", ["-x", artifact, extraction], "DEB extraction");
    verifyLinuxLayout(extraction, "usr", arch);
  } finally {
    rmSync(extraction, { recursive: true, force: true });
  }
}

function verifyRpm(artifact: string, arch: ReleaseArch): void {
  const packageName = run("rpm", ["-qp", "--qf", "%{NAME}", artifact], "RPM package query").trim();
  const packageArch = run("rpm", ["-qp", "--qf", "%{ARCH}", artifact], "RPM arch query").trim();
  if (packageName !== "dsh-desktop") throw new Error(`unexpected RPM package name: ${packageName}`);
  const expectedArch = arch === "x64" ? "x86_64" : "aarch64";
  if (packageArch !== expectedArch) {
    throw new Error(`RPM architecture ${packageArch} != expected ${expectedArch}`);
  }
  const extraction = join(tmpDir, `rpm-extract-${process.pid}`);
  rmSync(extraction, { recursive: true, force: true });
  mkdirSync(extraction, { recursive: true });
  try {
    run(
      "bash",
      [
        "-o",
        "pipefail",
        "-c",
        'rpm2cpio "$DSH_BUNDLE" | cpio -idm --quiet --no-absolute-filenames',
      ],
      "RPM extraction",
      { cwd: extraction, env: { ...process.env, DSH_BUNDLE: artifact } },
    );
    verifyLinuxLayout(extraction, "usr", arch);
  } finally {
    rmSync(extraction, { recursive: true, force: true });
  }
}

function verifyAppImage(artifact: string, arch: ReleaseArch): void {
  assertExecutable(artifact, "AppImage artifact");
  checkBinaryType(artifact, "ELF", arch, "AppImage runtime");
  const extraction = join(tmpDir, `appimage-extract-${process.pid}`);
  rmSync(extraction, { recursive: true, force: true });
  mkdirSync(extraction, { recursive: true });
  try {
    run(artifact, ["--appimage-extract"], "AppImage extraction", {
      cwd: extraction,
      env: { ...process.env, APPIMAGE_EXTRACT_AND_RUN: "1" },
    });
    const root = join(extraction, "squashfs-root");
    if (!existsSync(root)) throw new Error("AppImage extraction did not create squashfs-root");
    verifyLinuxLayout(root, "usr", arch);
  } finally {
    rmSync(extraction, { recursive: true, force: true });
  }
}

function verifyFlatpak(artifact: string, arch: ReleaseArch): void {
  const scratch = join(tmpDir, `flatpak-verify-${process.pid}`);
  const repository = join(scratch, "repo");
  const checkout = join(scratch, "checkout");
  rmSync(scratch, { recursive: true, force: true });
  mkdirSync(scratch, { recursive: true });
  try {
    run(
      "ostree",
      [`--repo=${repository}`, "init", "--mode=archive-z2"],
      "Flatpak verification repository initialization",
    );
    run("flatpak", ["build-import-bundle", repository, artifact], "Flatpak bundle import");
    const refs = run("ostree", [`--repo=${repository}`, "refs"], "Flatpak OSTree ref query")
      .split(/\r?\n/)
      .map((value) => value.trim())
      .filter((value) => value.startsWith(`app/${appIdentifier}/`));
    if (refs.length !== 1) {
      throw new Error(`expected one ${appIdentifier} Flatpak ref, found ${refs.join(", ") || "none"}`);
    }
    const expectedArch = arch === "x64" ? "x86_64" : "aarch64";
    if (!refs[0].includes(`/${expectedArch}/`)) {
      throw new Error(`Flatpak ref ${refs[0]} does not target ${expectedArch}`);
    }
    run(
      "ostree",
      [`--repo=${repository}`, "checkout", "--user-mode", refs[0], checkout],
      "Flatpak OSTree checkout",
    );
    verifyLinuxLayout(checkout, "files", arch);
    const metadata = readFileSync(join(checkout, "metadata"), "utf8");
    if (!metadata.includes(`name=${appIdentifier}`)) {
      throw new Error(`Flatpak metadata does not identify ${appIdentifier}`);
    }
    ok(`Flatpak metadata identifies ${appIdentifier}`);
    const metainfoPath = join(
      checkout,
      "export",
      "share",
      "metainfo",
      `${appIdentifier}.metainfo.xml`,
    );
    assertFile(metainfoPath, "Flatpak AppStream metadata");
    const metainfoProblems = flatpakMetadataProblems(readFileSync(metainfoPath, "utf8"));
    if (metainfoProblems.length > 0) throw new Error(metainfoProblems.join("\n"));
    ok("Flatpak AppStream identity and launchable are aligned");

    // flatpak-builder strips binaries by default. That would silently turn
    // this into a second, unreviewed runtime build even though the manifest
    // imports the verified DEB. Compare the complete runtime tree and main
    // executable bytes against that exact DEB input.
    const deb = artifactFor("deb");
    const debExtraction = join(scratch, "deb");
    mkdirSync(debExtraction, { recursive: true });
    run("dpkg-deb", ["-x", deb, debExtraction], "Flatpak source DEB extraction");
    run(
      "cmp",
      [
        "--silent",
        join(debExtraction, "usr", "bin", "deepseek-harness-desktop"),
        join(checkout, "files", "bin", "deepseek-harness-desktop"),
      ],
      "Flatpak main-binary byte comparison",
    );
    run(
      "diff",
      [
        "--no-dereference",
        "--recursive",
        join(debExtraction, "usr", "lib", productName, "runtime"),
        join(checkout, "files", "lib", productName, "runtime"),
      ],
      "Flatpak runtime byte comparison",
    );
    ok("Flatpak main binary and complete runtime are byte-identical to the DEB input");
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

function runSelfTest(): void {
  const fixture = [
    "7-Zip 24.09 : Copyright",
    "",
    "Listing archive: installer.exe",
    "--",
    "Path = D:\\a\\repo\\installer.exe",
    "----------",
    "Path = DSH Desktop.exe",
    "Path = runtime\\node.exe",
    "Path = runtime/harness/a b/bin.js",
  ].join("\n");
  const entries = parseSltListing(fixture);
  if (entries.join("|") !== "DSH Desktop.exe|runtime/node.exe|runtime/harness/a b/bin.js") {
    fail(`self-test: incorrect 7z listing parse: ${entries.join(", ")}`);
  }

  mkdirSync(tmpDir, { recursive: true });
  const executable = join(tmpDir, "bundle-executable-fixture");
  const plain = join(tmpDir, "bundle-plain-fixture");
  writeFileSync(executable, "fixture", { mode: 0o755 });
  writeFileSync(plain, "fixture", { mode: 0o644 });
  try {
    assertExecutable(executable, "executable fixture");
    let rejected = false;
    try {
      assertExecutable(plain, "plain fixture");
    } catch {
      rejected = true;
    }
    if (!rejected) fail("self-test: executable check accepted mode 0644");
  } finally {
    rmSync(executable, { force: true });
    rmSync(plain, { force: true });
  }
  ok("self-test: bundle parser and executable checks pass");
}

if (process.argv.includes("--self-test")) {
  runSelfTest();
  process.exit(0);
}

const bundle = parseBundle(argument("--bundle"));
const arch = parseArch(argument("--arch"));
const schemes = config.plugins?.["deep-link"]?.desktop?.schemes ?? [];
if (!schemes.includes("dsharness")) {
  fail(`tauri.conf.json must register dsharness, got ${schemes.join(", ") || "none"}`);
}
ok("tauri.conf.json registers the dsharness deep-link scheme");

const artifact = artifactFor(bundle);
info(`verifying ${bundle} artifact: ${artifact}`);
try {
  if (bundle === "nsis") verifyNsis(artifact, arch);
  else if (bundle === "msi") verifyMsi(artifact, arch);
  else if (bundle === "dmg") verifyDmg(artifact, arch);
  else if (bundle === "deb") verifyDeb(artifact, arch);
  else if (bundle === "rpm") verifyRpm(artifact, arch);
  else if (bundle === "appimage") verifyAppImage(artifact, arch);
  else verifyFlatpak(artifact, arch);
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}
ok(`${bundle} ${arch} bundle verification passed`);
