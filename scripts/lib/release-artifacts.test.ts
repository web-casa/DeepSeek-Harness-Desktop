import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  BUNDLE_SPECS,
  NATIVE_RELEASE_TARGETS,
  STORE_MSIX_TARGETS,
  githubNativeMatrix,
  githubMacosNotarizationMatrix,
  githubMsixMatrix,
  publishedWindowsNsisUpdaterPlatforms,
  publicArtifactsFor,
  releasePlanProblems,
} from "./release-artifacts.ts";

test("release plan covers every requested public format exactly through reviewed targets", () => {
  assert.deepEqual(releasePlanProblems(), []);
  const formats = new Set(NATIVE_RELEASE_TARGETS.flatMap((target) => target.bundles));
  assert.deepEqual([...formats].sort(), Object.keys(BUNDLE_SPECS).sort());
  assert.equal(formats.has("flatpak"), true);
  assert.equal([...formats].includes("msix" as never), false);
});

test("native matrix uses current architecture-specific hosted runners", () => {
  const rows = githubNativeMatrix().include;
  assert.equal(rows.length, 6);
  assert.equal(rows.some((row) => row.os === "macos-14"), false);
  assert.equal(rows.some((row) => row.os === "macos-15-intel"), true);
  assert.equal(rows.some((row) => row.os === "ubuntu-22.04-arm"), true);
  assert.equal(rows.some((row) => row.os === "windows-11-arm"), true);
  assert.deepEqual(
    rows.map((row) => [row.target, row.hostTriple]),
    [
      ["windows-x64", "x86_64-pc-windows-msvc"],
      ["windows-arm64", "aarch64-pc-windows-msvc"],
      ["macos-arm64", "aarch64-apple-darwin"],
      ["macos-x64", "x86_64-apple-darwin"],
      ["linux-x64", "x86_64-unknown-linux-gnu"],
      ["linux-arm64", "aarch64-unknown-linux-gnu"],
    ],
  );
  for (const row of rows.filter((candidate) =>
    String(candidate.target).startsWith("macos-"),
  )) {
    assert.equal(row.tauriBundles, "app");
  }
});

test("Windows public artifact contract expands only MSI installer UI locales", () => {
  const windows = NATIVE_RELEASE_TARGETS.filter((target) => target.id.startsWith("windows-"));
  assert.equal(windows.length, 2);
  for (const target of windows) {
    assert.deepEqual(publicArtifactsFor(target), [
      { bundle: "nsis" },
      { bundle: "msi", installerLocale: "en-US" },
      { bundle: "msi", installerLocale: "zh-CN" },
    ]);
  }
  for (const target of NATIVE_RELEASE_TARGETS.filter((target) => !target.id.startsWith("windows-"))) {
    assert.equal(publicArtifactsFor(target).some((artifact) => artifact.installerLocale), false);
  }
});

test("manual native selection is exact and keeps notarization aligned", () => {
  assert.deepEqual(
    githubNativeMatrix("macos-x64").include.map((row) => row.target),
    ["macos-x64"],
  );
  assert.deepEqual(
    githubMacosNotarizationMatrix("macos-x64").include.map((row) => row.target),
    ["macos-x64"],
  );
  assert.throws(() => githubNativeMatrix("macos-unreviewed"), /unknown native release target/);
});

test("macOS notarization handoff is private and derived from the native matrix", () => {
  const rows = githubMacosNotarizationMatrix().include;
  assert.deepEqual(
    rows.map((row) => row.target),
    ["macos-arm64", "macos-x64"],
  );
  for (const row of rows) {
    assert.match(String(row.handoffArtifact), /^dsh-macos-notarization-/);
    assert.doesNotMatch(String(row.handoffArtifact), /^deepseek-harness-desktop-/);
    assert.match(String(row.artifact), /^deepseek-harness-desktop-macos-/);
  }
});

test("Store MSIX remains a separate exact x64 and arm64 matrix", () => {
  assert.deepEqual(
    STORE_MSIX_TARGETS.map((target) => target.arch),
    ["x64", "arm64"],
  );
  assert.equal(
    NATIVE_RELEASE_TARGETS.some((target) => target.artifact.includes("store-msix")),
    false,
  );
  assert.deepEqual(
    githubMsixMatrix().include.map((target) => target.artifact),
    ["dsh-desktop-store-msix-x64", "dsh-desktop-store-msix-arm64"],
  );
  assert.deepEqual(
    githubMsixMatrix().include.map((target) => target.nativeTarget),
    ["windows-x64", "windows-arm64"],
  );
});

test("every public artifact path includes both installer and SHA-256 sidecar", () => {
  for (const target of NATIVE_RELEASE_TARGETS) {
    for (const bundle of target.bundles) {
      const spec = BUNDLE_SPECS[bundle];
      assert.equal(
        target.uploadPaths.includes(`${spec.directory}/*${spec.suffix}`),
        true,
      );
      assert.equal(
        target.uploadPaths.includes(`${spec.directory}/*${spec.suffix}.sha256`),
        true,
      );
    }
  }
});

test("in-app updates publish only exact NSIS architecture targets", () => {
  assert.deepEqual(publishedWindowsNsisUpdaterPlatforms(), [
    "windows-x86_64-nsis",
    "windows-aarch64-nsis",
  ]);
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(
    workflow,
    /--platforms windows-x86_64-nsis,windows-aarch64-nsis/,
  );
  assert.doesNotMatch(workflow, /--platforms windows-x86_64(?:\s|$)/);
});

test("both reusable quality jobs checkout the requested release revision", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/quality.yml", import.meta.url),
    "utf8",
  );
  const exactRef = "ref: ${{ inputs.checkout_ref || github.ref }}";
  assert.equal(workflow.split(exactRef).length - 1, 2);
});

test("Windows installer smoke searches the preserved artifact tree exactly", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(
    workflow,
    /Get-ChildItem -Path artifacts -Recurse -File -Filter '\*-setup\.exe'/,
  );
  assert.match(
    workflow,
    /Get-ChildItem -Path artifacts -Recurse -File -Filter '\*\.msi'/,
  );
  // There are two exact NSIS selections: initial deep-link launch and the
  // independent in-place reinstall/runtime smoke. The third is the MSI job.
  assert.equal(workflow.split("$installers.Count -ne 1").length - 1, 3);
  assert.equal(
    workflow.split("Get-ChildItem -Path artifacts -Recurse -File -Filter '*-setup.exe'").length - 1,
    2,
  );
  assert.match(workflow, /In-place NSIS reinstall and installed Harness smoke/);
  assert.match(workflow, /installed Harness never became ready/);
  // PowerShell single-quoted strings do not consume backslashes. The pattern
  // must escape the dot once, not twice (which looks for a literal backslash
  // before an arbitrary character). Exercise the
  // exact registration format emitted by the NSIS installer so every native
  // deep-link smoke cannot regress into a false failure.
  const nsisProtocolPattern = String.raw`'^"([^"]+\.exe)"\s+"%1"$'`;
  const accidentalDoubleEscape = String.raw`'^"([^"]+\\.exe)"\s+"%1"$'`;
  assert.equal(workflow.includes(nsisProtocolPattern), true);
  assert.equal(workflow.includes(accidentalDoubleEscape), false);
  const registeredCommand =
    '"C:\\Users\\runneradmin\\AppData\\Local\\DSH Desktop\\deepseek-harness-desktop.exe" "%1"';
  assert.equal(
    new RegExp(nsisProtocolPattern.slice(1, -1), "i").test(registeredCommand),
    true,
  );
  assert.equal(
    new RegExp(accidentalDoubleEscape.slice(1, -1), "i").test(registeredCommand),
    false,
  );
  // `Split-Path -LiteralPath ... -Parent` is not a valid PowerShell parameter
  // set. The native path API preserves literal handling while accepting a
  // fully-qualified executable path from the validated protocol registration.
  assert.equal(workflow.includes("[System.IO.Path]::GetDirectoryName($desktopExe)"), true);
  assert.equal(workflow.includes("Split-Path -LiteralPath $desktopExe -Parent"), false);
  assert.match(
    workflow,
    /registered desktop executable has no parent directory: \$desktopExe/,
  );
  assert.match(workflow, /\$_.Name\.EndsWith\("_\$locale\.msi", \[System\.StringComparison\]::Ordinal\)/);
  assert.match(workflow, /MSI ProductLanguage \$productLanguage does not match \$locale/);
  assert.match(workflow, /\$quotedInstaller = '\"' \+ \$installer\.FullName \+ '\"'/);
  assert.match(workflow, /\/l\*v \$quotedLogPath/);
  assert.match(workflow, /\$process\.WaitForExit\(600000\)/);
});

test("Windows installer smoke can safely reuse one completed Release run", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(workflow, /windows_smoke_source_run_id:/);
  assert.match(workflow, /windows_smoke_installer:/);
  assert.match(workflow, /actions: read/);
  assert.match(
    workflow,
    /source_workflow" != "\.github\/workflows\/release\.yml"/,
  );
  assert.match(
    workflow,
    /source_status" != "completed"/,
  );
  assert.match(workflow, /deepseek-harness-desktop-windows-x64/);
  assert.match(workflow, /deepseek-harness-desktop-windows-arm64/);
  assert.match(workflow, /jq --arg name "\$artifact_name"/);
  const sourceRun =
    "run-id: ${{ github.event.inputs.windows_smoke_source_run_id || github.run_id }}";
  assert.equal(workflow.split(sourceRun).length - 1, 2);
  assert.equal(
    workflow.split("needs: [build, windows-smoke-source]").length - 1,
    2,
  );
  assert.match(
    workflow,
    /github\.event\.inputs\.windows_smoke_installer != 'msi'/,
  );
  assert.match(
    workflow,
    /github\.event\.inputs\.windows_smoke_installer != 'nsis'/,
  );
  const readOnlySourceRunGate =
    "if: github.event_name != 'workflow_dispatch' || github.event.inputs.windows_smoke_source_run_id == ''";
  const presetContract = workflow.slice(
    workflow.indexOf("  preset-download-contract:"),
    workflow.indexOf("\n  quality:"),
  );
  const quality = workflow.slice(
    workflow.indexOf("  quality:"),
    workflow.indexOf("\n  # Refuse to spend two platform builds"),
  );
  assert.ok(presetContract.includes(readOnlySourceRunGate));
  assert.ok(quality.includes(readOnlySourceRunGate));
});

test("release host and installer smokes cover each declared native architecture", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  // GitHub expands these reviewed matrix values before selecting the runner
  // shell. Do not route them through `$RELEASE_TARGET`: Windows defaults to
  // PowerShell, where environment variables use `$env:RELEASE_TARGET`.
  assert.match(
    workflow,
    /node scripts\/verify-native-host\.ts --target "\$\{\{ matrix\.target \}\}"/,
  );
  assert.match(
    workflow,
    /node scripts\/verify-native-host\.ts --target "\$\{\{ matrix\.nativeTarget \}\}"/,
  );
  assert.doesNotMatch(
    workflow,
    /verify-native-host\.ts --target "\$RELEASE_TARGET"/,
  );
  assert.match(workflow, /os: windows-11-arm/);
  assert.match(workflow, /artifact: deepseek-harness-desktop-windows-arm64/);
  assert.match(workflow, /x64 compatibility on ARM64/);
  assert.match(workflow, /matrix: \$\{\{ fromJSON\(needs\.release-plan\.outputs\.macos_notarization\) \}\}/);
  assert.match(workflow, /runs-on: \$\{\{ matrix\.os \}\}/);
  assert.match(workflow, /retention-days: 14/);
  assert.match(workflow, /Require public-repository free runner policy/);
});

test("tag publication tolerates only the intentional transitive smoke-source skip", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  const releaseJob = workflow.slice(workflow.indexOf("\n  release:\n"));
  assert.match(releaseJob, /always\(\)/);
  assert.match(releaseJob, /github\.event_name == 'push'/);
  assert.match(releaseJob, /startsWith\(github\.ref, 'refs\/tags\/'\)/);
  for (const dependency of [
    "build",
    "notarize-macos",
    "build-msix",
    "soak",
    "tag-gate",
    "windows-deep-link-smoke",
    "windows-msi-smoke",
    "macos-deep-link-smoke",
    "preset-download-contract",
  ]) {
    assert.equal(
      releaseJob.includes(`needs.${dependency}.result == 'success'`),
      true,
      dependency,
    );
  }
});

test("release shell never interpolates the attacker-controlled ref name", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.equal(workflow.includes("--expect-tag ${{ github.ref_name }}"), false);
  assert.equal(workflow.includes("--tag ${{ github.ref_name }}"), false);
  assert.equal(workflow.includes("tag ${{ github.ref_name }} is not"), false);
  assert.match(workflow, /RELEASE_TAG: \$\{\{ github\.ref_name \}\}/);
  assert.match(workflow, /\^v\(0\|\[1-9\]\[0-9\]\*\)\\\./);
  assert.match(workflow, /--expect-tag "\$RELEASE_TAG"/);
  assert.match(workflow, /--tag "\$RELEASE_TAG"/);
});

test("release matrix treats workflow native_target as data, not shell source", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  const start = workflow.indexOf("      - name: Emit reviewed native, notarization, and Store matrices");
  const end = workflow.indexOf("\n  # The desktop deliberately refuses redirects", start);
  assert.ok(start >= 0 && end > start, "release matrix step missing");
  const step = workflow.slice(start, end);
  const shell = step.slice(step.indexOf("        run: |"));

  assert.match(
    step,
    /NATIVE_TARGET: \$\{\{ github\.event\.inputs\.native_target \|\| 'all' \}\}/,
  );
  assert.doesNotMatch(shell, /\$\{\{/);
  assert.equal(
    shell.split('--target "$NATIVE_TARGET"').length - 1,
    2,
    "both target-dependent matrices must use the quoted environment value",
  );
  assert.equal(
    shell.includes("--target '${{ github.event.inputs.native_target"),
    false,
  );
});

test("manual Release always builds reviewed main, never a dispatch-selected source ref", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  const safeRef =
    "ref: ${{ github.event_name == 'workflow_dispatch' && 'refs/heads/main' || github.ref }}";
  const releasePlanStart = workflow.indexOf("  release-plan:");
  const releasePlanEnd = workflow.indexOf("\n  # The desktop deliberately refuses redirects", releasePlanStart);
  const releasePlan = workflow.slice(releasePlanStart, releasePlanEnd);
  const checkoutCount = workflow.split("uses: actions/checkout@").length - 1;
  const checkoutRefs = [
    ...workflow.matchAll(
      /uses: actions\/checkout@[^\n]+\n\s+with:\n\s+ref: ([^\n]+)/g,
    ),
  ].map((match) => `ref: ${match[1]}`);

  assert.ok(checkoutCount > 0, "Release must retain explicit pinned checkouts");
  assert.match(
    releasePlan,
    /if: github\.event_name != 'workflow_dispatch' \|\| \(github\.ref == 'refs\/heads\/main' && github\.event\.inputs\.windows_smoke_source_run_id == ''\)/,
    "manual dispatches outside main must not enter the signing/build dependency graph",
  );
  assert.equal(
    workflow.includes("github.event.inputs.tag"),
    false,
    "a dispatch input must never choose source executed with release credentials",
  );
  assert.equal(
    checkoutRefs.length,
    checkoutCount,
    "every checkout step must declare an explicit ref",
  );
  assert.deepEqual(
    checkoutRefs,
    Array.from({ length: checkoutCount }, () => safeRef),
    "every Release checkout must bind dispatches to main and tags to their tag ref",
  );
  assert.match(
    workflow,
    /checkout_ref: \$\{\{ github\.event_name == 'workflow_dispatch' && 'refs\/heads\/main' \|\| github\.ref \}\}/,
  );
});

test("dependency review is blocking with a graph-unavailable fallback", () => {
  const dependencyWorkflow = readFileSync(
    new URL("../../.github/workflows/dependency-review.yml", import.meta.url),
    "utf8",
  );
  assert.equal(dependencyWorkflow.includes("continue-on-error"), false);
  assert.match(dependencyWorkflow, /fail-on-severity: low/);
  assert.match(
    dependencyWorkflow,
    /dependency-graph\/compare\/\$BASE_SHA\.\.\.\$HEAD_SHA/,
  );
  assert.match(dependencyWorkflow, /403\|404\)/);
  assert.match(dependencyWorkflow, /pnpm audit --audit-level low/);
  assert.match(dependencyWorkflow, /npm audit --audit-level=low/);
  assert.match(
    dependencyWorkflow,
    /cargo metadata --locked --all-features --format-version 1 > \/dev\/null/,
  );
  assert.doesNotMatch(dependencyWorkflow, /cargo metadata --locked --format-version 1 --no-deps/);
  assert.match(dependencyWorkflow, /git diff --exit-code -- Cargo\.lock/);
  assert.match(dependencyWorkflow, /cargo vet --locked/);
  assert.match(dependencyWorkflow, /verify-js-licenses\.ts --format pnpm/);

  const qualityWorkflow = readFileSync(
    new URL("../../.github/workflows/quality.yml", import.meta.url),
    "utf8",
  );
  assert.match(
    qualityWorkflow,
    /cargo metadata --locked --all-features --format-version 1/,
  );
  assert.match(qualityWorkflow, /git diff --exit-code -- Cargo\.lock/);
});

test("zip 8 selects an explicit portable flate2 backend on every target", () => {
  const cargo = readFileSync(new URL("../../src-tauri/Cargo.toml", import.meta.url), "utf8");
  // zip's deflate-flate2 feature enables its API but deliberately leaves the
  // flate2 backend unset. Linux happened to receive one through PNG; Windows
  // did not, so keep the application-level pure-Rust choice contractual.
  assert.match(
    cargo,
    /zip = \{ version = "8", default-features = false, features = \["deflate-flate2"\] \}/,
  );
  assert.match(
    cargo,
    /flate2 = \{ version = "1\.1\.9", default-features = false, features = \["rust_backend"\] \}/,
  );
  assert.doesNotMatch(cargo, /deflate-flate2-zlib|deflate-flate2-zlib-ng|deflate-zopfli/);
});

test("release verifies external contracts before publishing", () => {
  const releaseWorkflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(releaseWorkflow, /node scripts\/verify-cordis-market-api\.ts/);
  const inventory = releaseWorkflow.indexOf(
    "run: node scripts/verify-release-inventory.ts --directory artifacts",
  );
  const signatures = releaseWorkflow.indexOf(
    "run: node scripts/verify-updater-signatures.ts --directory artifacts",
  );
  const publish = releaseWorkflow.indexOf("uses: softprops/action-gh-release@");
  assert.equal(inventory > 0 && signatures > inventory && publish > signatures, true);
});

test("release publishes its draft only after the exact updater manifest is uploaded", () => {
  const releaseWorkflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  const draft = releaseWorkflow.indexOf("draft: true");
  const manifest = releaseWorkflow.indexOf("name: Publish updater manifest (latest.json)");
  const publish = releaseWorkflow.indexOf("name: Publish reviewed GitHub Release");
  assert.equal(draft > 0 && manifest > draft && publish > manifest, true);
  const finalStep = releaseWorkflow.slice(publish);
  assert.match(finalStep, /gh release edit "\$RELEASE_TAG" --draft=false/);
  assert.match(finalStep, /RELEASE_TAG: \$\{\{ github\.ref_name \}\}/);
  assert.match(finalStep, /GH_TOKEN: \$\{\{ github\.token \}\}/);
});

test("MSI smoke accepts a valid 8.3 registry path but verifies the real file", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(workflow, /Test-Path -LiteralPath \$registeredPath -PathType Leaf/);
  assert.match(
    workflow,
    /\[System\.IO\.Path\]::GetFileName\(\$installedBinary\.FullName\)/,
  );
  assert.match(
    workflow,
    /\$installedFileName -ine 'deepseek-harness-desktop\.exe'/,
  );
  assert.doesNotMatch(
    workflow,
    /\$command -notlike '\*deepseek-harness-desktop\.exe\*'/,
  );
});

test("macOS release signs runtime, uploads once, then waits in a separate job", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  const importIndex = workflow.indexOf("run: node scripts/import-apple-certificate.ts");
  const nestedIndex = workflow.indexOf("run: node scripts/sign-macos-runtime.ts");
  const postSignSmokeIndex = workflow.indexOf("name: Runtime smoke (post-sign)");
  const bundleIndex = workflow.indexOf("name: Bundle macOS app (signed; notarization deferred)");
  const dmgIndex = workflow.indexOf("name: Build macOS DMG without Finder automation");
  const preVerifyIndex = workflow.indexOf("name: Verify signed DMG before notarization");
  const preSubmitCleanupIndex = workflow.indexOf(
    "name: Remove job-scoped macOS signing keychain before submission",
  );
  const submitIndex = workflow.indexOf("name: Submit signed DMG without waiting");
  const handoffIndex = workflow.indexOf("name: Upload notarization handoff");
  const waitJobIndex = workflow.indexOf("notarize-macos:");
  const waitIndex = workflow.indexOf("name: Wait for the recorded Apple submission");
  const cleanupIndex = workflow.indexOf("name: Remove remaining job-scoped macOS signing keychain");
  assert.equal(importIndex > 0, true);
  assert.equal(nestedIndex > importIndex, true);
  assert.equal(postSignSmokeIndex > nestedIndex, true);
  assert.equal(bundleIndex > postSignSmokeIndex, true);
  assert.equal(dmgIndex > bundleIndex, true);
  assert.equal(preVerifyIndex > dmgIndex, true);
  assert.equal(preSubmitCleanupIndex > preVerifyIndex, true);
  assert.equal(submitIndex > preSubmitCleanupIndex, true);
  assert.equal(submitIndex > bundleIndex, true);
  assert.equal(handoffIndex > submitIndex, true);
  assert.equal(waitJobIndex > handoffIndex, true);
  assert.equal(waitIndex > waitJobIndex, true);
  assert.equal(cleanupIndex > bundleIndex, true);
  assert.match(workflow, /run: node scripts\/macos-notarization\.ts submit/);
  assert.match(workflow, /run: node scripts\/macos-notarization\.ts wait/);
  assert.match(workflow, /run: node scripts\/build-macos-dmg\.ts --arch/);
  assert.match(workflow, /native_target:/);
  assert.match(workflow, /--github-matrix --target/);

  const bundleVerifier = readFileSync(new URL("../verify-bundle.ts", import.meta.url), "utf8");
  assert.match(bundleVerifier, /DMG-contained app codesign verification/);
  assert.match(bundleVerifier, /readlinkSync\(applicationsLink\) !== "\/Applications"/);

  // Tauri must use the already-imported identity. Passing the PKCS#12 again
  // creates a separate process-scoped keychain that the nested signer cannot
  // access and makes the two signing paths needlessly diverge.
  const signedBundleStep = workflow.slice(bundleIndex, preVerifyIndex);
  assert.equal(signedBundleStep.includes("APPLE_CERTIFICATE:"), false);
  assert.equal(signedBundleStep.includes("APPLE_ID:"), false);
  assert.equal(signedBundleStep.includes("APPLE_PASSWORD:"), false);
  const importer = readFileSync(
    new URL("../import-apple-certificate.ts", import.meta.url),
    "utf8",
  );
  assert.equal(importer.includes("APPLE_SIGNING_IDENTITY=${identity}"), true);
  assert.equal(workflow.slice(cleanupIndex).includes("always()"), true);
  assert.equal(workflow.slice(cleanupIndex).includes("::warning::"), true);
});
