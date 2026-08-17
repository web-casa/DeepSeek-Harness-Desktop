// Verify the MSIX packages produced by tauri-windows-bundle for Store
// submission. Runs on the Windows MSIX build job.
//
// Checks:
//   * x64 and arm64 .msix packages exist
//   * package identity matches the reserved Partner Center identity
//   * the main executable and staged runtime are inside the package
//   * the dsharness:// protocol and runFullTrust capability are declared
//
//   node scripts/verify-msix.ts

import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { repoRoot, fail, ok } from "./lib/common.ts";

const PACKAGE_NAME = "53660AlanM.DSHDesktopCommunity";
const PUBLISHER = "CN=84AC3716-04E0-4D67-8951-0D3E51674CA0";
const PROTOCOL = "dsharness";

const archIdx = process.argv.indexOf("--arch");
const requestedArch = archIdx >= 0 ? process.argv[archIdx + 1] : undefined;
if (requestedArch !== undefined && requestedArch !== "x64" && requestedArch !== "arm64") {
  fail("--arch must be x64 or arm64");
}

const msixDir = join(repoRoot, "target", "msix");
if (!existsSync(msixDir)) fail(`msix dir missing at ${msixDir}`);

function packagesFor(arch: "x64" | "arm64"): string[] {
  return readdirSync(msixDir)
    .filter((f) => f.toLowerCase().endsWith(".msix"))
    .filter((f) => f.includes(`_${arch}`) || f.includes(`-${arch}`))
    .map((f) => join(msixDir, f));
}

const targets: { arch: "x64" | "arm64"; packages: string[] }[] = [];
if (requestedArch === undefined || requestedArch === "x64") {
  targets.push({ arch: "x64", packages: packagesFor("x64") });
}
if (requestedArch === undefined || requestedArch === "arm64") {
  targets.push({ arch: "arm64", packages: packagesFor("arm64") });
}
for (const target of targets) {
  if (target.packages.length !== 1) {
    fail(`expected exactly one ${target.arch} msix, found: ${target.packages.join(", ") || "none"}`);
  }
}

function readZipEntries(msix: string): { name: string; content?: string }[] {
  const ps = `
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($env:DSH_MSIX)
foreach ($entry in $zip.Entries) {
    $name = $entry.FullName
    $content = $null
    if ($name -eq "AppxManifest.xml") {
      $reader = New-Object System.IO.StreamReader($entry.Open(), [System.Text.Encoding]::UTF8)
      $content = $reader.ReadToEnd()
      $reader.Close()
    }
    [pscustomobject]@{ name = $name; content = $content } | ConvertTo-Json -Compress
  }
  $zip.Dispose()
`;
  const res = spawnSync("powershell", ["-NoProfile", "-Command", ps], {
    encoding: "utf8",
    env: { ...process.env, DSH_MSIX: msix },
    maxBuffer: 16 * 1024 * 1024,
  });
  if (res.status !== 0) {
    fail(`failed to list ${msix}: ${(res.stderr ?? res.stdout ?? "").trim()}`);
  }
  const parsed: { name: string; content?: string }[] = [];
  for (const line of (res.stdout ?? "").split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const obj = JSON.parse(line) as { name?: string; content?: string | null };
      if (obj.name) parsed.push({ name: obj.name, content: obj.content ?? undefined });
    } catch {
      // Ignore non-JSON progress/format lines emitted by PowerShell hosts.
    }
  }
  return parsed;
}

for (const target of targets) {
  const msix = target.packages[0];
  const entries = readZipEntries(msix);
  const names = new Set(entries.map((e) => e.name.replace(/\\/g, "/")));
  const manifest = entries.find((e) => e.name === "AppxManifest.xml")?.content ?? "";
  for (const required of [
    "deepseek-harness-desktop.exe",
    "runtime/node.exe",
    "runtime/sidecar.exe",
    "runtime/harness/runtime-manifest.json",
  ]) {
    if (!names.has(required)) fail(`${msix} is missing ${required}`);
  }
  if (!manifest.includes(`Name="${PACKAGE_NAME}"`)) {
    fail(`${msix} manifest identity name mismatch`);
  }
  if (!manifest.includes(`Publisher="${PUBLISHER}"`)) {
    fail(`${msix} manifest publisher mismatch`);
  }
  if (!manifest.includes(`Name="${PROTOCOL}"`) || !manifest.includes("windows.protocol")) {
    fail(`${msix} manifest is missing the ${PROTOCOL}:// protocol extension`);
  }
  if (!manifest.includes('Name="runFullTrust"')) {
    fail(`${msix} manifest is missing runFullTrust`);
  }
  ok(`${msix} verified (${names.size} entries, identity OK, protocol OK)`);
}

if (process.argv.includes("--self-test")) {
  if (PACKAGE_NAME !== "53660AlanM.DSHDesktopCommunity") fail("self-test: package name changed");
  ok("self-test: verify-msix constants");
  process.exit(0);
}

ok("MSIX packages are Store-ready (static checks)");
