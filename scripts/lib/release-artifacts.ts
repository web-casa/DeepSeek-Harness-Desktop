import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import {
  WINDOWS_WIX_INSTALLER_LOCALES,
  type WindowsWixInstallerLocale,
  wixInstallerLocaleFromMsiName,
} from "./windows-installer-locales.ts";

export type ReleaseArch = "x64" | "arm64";
// Tauri resolves updater targets as `<os>-<arch>-<installer>`.  Keep the
// installer suffix explicit: a generic `windows-x86_64` entry would let an
// MSI install silently consume an NSIS update (or vice versa).
export const WINDOWS_NSIS_UPDATER_PLATFORMS = [
  "windows-x86_64-nsis",
  "windows-aarch64-nsis",
] as const;
export type WindowsNsisUpdaterPlatform = (typeof WINDOWS_NSIS_UPDATER_PLATFORMS)[number];
export type PublicBundle =
  | "nsis"
  | "msi"
  | "dmg"
  | "appimage"
  | "deb"
  | "rpm"
  | "flatpak";

// `app` is an internal macOS bundle target used to materialize the signed app
// and updater tripwire. The public DMG is built separately without Finder
// automation; users never receive the raw app bundle.
export type TauriBundle = Exclude<PublicBundle, "flatpak"> | "app";

export interface BundleSpec {
  directory: string;
  suffix: string;
  signing: "authenticode" | "apple" | "checksum";
}

export const BUNDLE_SPECS: Readonly<Record<PublicBundle, BundleSpec>> = {
  nsis: {
    directory: "target/release/bundle/nsis",
    suffix: "-setup.exe",
    signing: "authenticode",
  },
  msi: {
    directory: "target/release/bundle/msi",
    suffix: ".msi",
    signing: "authenticode",
  },
  dmg: {
    directory: "target/release/bundle/dmg",
    suffix: ".dmg",
    signing: "apple",
  },
  appimage: {
    directory: "target/release/bundle/appimage",
    suffix: ".AppImage",
    signing: "checksum",
  },
  deb: {
    directory: "target/release/bundle/deb",
    suffix: ".deb",
    signing: "checksum",
  },
  rpm: {
    directory: "target/release/bundle/rpm",
    suffix: ".rpm",
    signing: "checksum",
  },
  flatpak: {
    directory: "target/release/bundle/flatpak",
    suffix: ".flatpak",
    signing: "checksum",
  },
};

export interface NativeReleaseTarget {
  id: string;
  os: string;
  arch: ReleaseArch;
  // The Rust host triple must match the runner. Native release jobs do not
  // cross-compile the Desktop payload or its bundled runtime.
  hostTriple: string;
  bundles: readonly PublicBundle[];
  // WiX produces one MSI per installer locale. This is deliberately absent
  // for non-Windows targets, where a locale is not part of the filename.
  msiLocales?: readonly WindowsWixInstallerLocale[];
  tauriBundles: readonly TauriBundle[];
  artifact: string;
  uploadPaths: readonly string[];
  appImageTools: boolean;
  flatpak: boolean;
  notarizationArtifact?: string;
  updaterSignature?: {
    directory: string;
    suffix: string;
    publish: boolean;
    // Present only for a signature that is a publicly selectable updater
    // payload. macOS keeps a build-only tripwire while its updater remains
    // deliberately disabled.
    platform?: WindowsNsisUpdaterPlatform;
  };
}

export interface PublicArtifact {
  bundle: PublicBundle;
  installerLocale?: WindowsWixInstallerLocale;
}

const artifactPaths = (bundles: readonly PublicBundle[]): string[] =>
  bundles.flatMap((bundle) => {
    const spec = BUNDLE_SPECS[bundle];
    return [
      `${spec.directory}/*${spec.suffix}`,
      `${spec.directory}/*${spec.suffix}.sha256`,
    ];
  });

export const NATIVE_RELEASE_TARGETS: readonly NativeReleaseTarget[] = [
  {
    id: "windows-x64",
    os: "windows-latest",
    arch: "x64",
    hostTriple: "x86_64-pc-windows-msvc",
    bundles: ["nsis", "msi"],
    msiLocales: WINDOWS_WIX_INSTALLER_LOCALES,
    tauriBundles: ["nsis", "msi"],
    artifact: "deepseek-harness-desktop-windows-x64",
    uploadPaths: [
      ...artifactPaths(["nsis", "msi"]),
      "target/release/bundle/nsis/*.sig",
    ],
    appImageTools: false,
    flatpak: false,
    updaterSignature: {
      directory: "target/release/bundle/nsis",
      suffix: ".sig",
      publish: true,
      platform: "windows-x86_64-nsis",
    },
  },
  {
    id: "windows-arm64",
    os: "windows-11-arm",
    arch: "arm64",
    hostTriple: "aarch64-pc-windows-msvc",
    bundles: ["nsis", "msi"],
    msiLocales: WINDOWS_WIX_INSTALLER_LOCALES,
    tauriBundles: ["nsis", "msi"],
    artifact: "deepseek-harness-desktop-windows-arm64",
    uploadPaths: [
      ...artifactPaths(["nsis", "msi"]),
      "target/release/bundle/nsis/*.sig",
    ],
    appImageTools: false,
    flatpak: false,
    // ARM64 uses its own exact NSIS target.  The public-release inventory and
    // manifest generator both fail closed if this signature is absent.
    updaterSignature: {
      directory: "target/release/bundle/nsis",
      suffix: ".sig",
      publish: true,
      platform: "windows-aarch64-nsis",
    },
  },
  {
    id: "macos-arm64",
    os: "macos-15",
    arch: "arm64",
    hostTriple: "aarch64-apple-darwin",
    bundles: ["dmg"],
    tauriBundles: ["app"],
    artifact: "deepseek-harness-desktop-macos-arm64",
    uploadPaths: [
      ...artifactPaths(["dmg"]),
    ],
    appImageTools: false,
    flatpak: false,
    notarizationArtifact: "dsh-macos-notarization-arm64",
    updaterSignature: {
      directory: "target/release/bundle/macos",
      suffix: ".app.tar.gz.sig",
      publish: false,
    },
  },
  {
    id: "macos-x64",
    os: "macos-15-intel",
    arch: "x64",
    hostTriple: "x86_64-apple-darwin",
    bundles: ["dmg"],
    tauriBundles: ["app"],
    artifact: "deepseek-harness-desktop-macos-x64",
    uploadPaths: [
      ...artifactPaths(["dmg"]),
    ],
    appImageTools: false,
    flatpak: false,
    notarizationArtifact: "dsh-macos-notarization-x64",
    updaterSignature: {
      directory: "target/release/bundle/macos",
      suffix: ".app.tar.gz.sig",
      publish: false,
    },
  },
  {
    id: "linux-x64",
    os: "ubuntu-22.04",
    arch: "x64",
    hostTriple: "x86_64-unknown-linux-gnu",
    bundles: ["appimage", "deb", "rpm", "flatpak"],
    tauriBundles: ["appimage", "deb", "rpm"],
    artifact: "deepseek-harness-desktop-linux-x64",
    uploadPaths: artifactPaths(["appimage", "deb", "rpm", "flatpak"]),
    appImageTools: true,
    flatpak: true,
  },
  {
    id: "linux-arm64",
    os: "ubuntu-22.04-arm",
    arch: "arm64",
    hostTriple: "aarch64-unknown-linux-gnu",
    bundles: ["appimage", "deb", "rpm", "flatpak"],
    tauriBundles: ["appimage", "deb", "rpm"],
    artifact: "deepseek-harness-desktop-linux-arm64",
    uploadPaths: artifactPaths(["appimage", "deb", "rpm", "flatpak"]),
    appImageTools: true,
    flatpak: true,
  },
] as const;

export const STORE_MSIX_TARGETS = [
  {
    arch: "x64" as const,
    os: "windows-latest",
    target: "x86_64-pc-windows-msvc",
    nativeTarget: "windows-x64",
  },
  {
    arch: "arm64" as const,
    os: "windows-11-arm",
    target: "aarch64-pc-windows-msvc",
    nativeTarget: "windows-arm64",
  },
] as const;

export function publicArtifactsFor(target: NativeReleaseTarget): readonly PublicArtifact[] {
  const artifacts: PublicArtifact[] = [];
  for (const bundle of target.bundles) {
    if (bundle !== "msi") {
      artifacts.push({ bundle });
      continue;
    }
    for (const installerLocale of target.msiLocales ?? []) {
      artifacts.push({ bundle, installerLocale });
    }
  }
  return artifacts;
}

export function releasePlanProblems(): string[] {
  const problems: string[] = [];
  const ids = new Set<string>();
  const artifacts = new Set<string>();
  const formats = new Set<PublicBundle>();
  const updaterPlatforms = new Set<WindowsNsisUpdaterPlatform>();

  for (const target of NATIVE_RELEASE_TARGETS) {
    if (ids.has(target.id)) problems.push(`duplicate target id: ${target.id}`);
    ids.add(target.id);
    if (artifacts.has(target.artifact)) {
      problems.push(`duplicate artifact name: ${target.artifact}`);
    }
    artifacts.add(target.artifact);
    if (!target.artifact.startsWith("deepseek-harness-desktop-")) {
      problems.push(`public artifact has unsafe namespace: ${target.artifact}`);
    }
    for (const bundle of target.bundles) formats.add(bundle);
    if (target.hostTriple.length === 0) {
      problems.push(`${target.id}: native host triple is required`);
    }
    if (target.bundles.includes("msi")) {
      if (!target.id.startsWith("windows-")) {
        problems.push(`${target.id}: MSI is only valid for Windows targets`);
      }
      if (
        target.msiLocales?.join(",") !== WINDOWS_WIX_INSTALLER_LOCALES.join(",")
      ) {
        problems.push(`${target.id}: MSI locales must exactly match the reviewed WiX locale list`);
      }
    } else if (target.msiLocales !== undefined) {
      problems.push(`${target.id}: msiLocales is invalid without an MSI bundle`);
    }
    if (target.bundles.includes("flatpak") !== target.flatpak) {
      problems.push(`${target.id}: flatpak flag does not match bundle set`);
    }
    if (target.bundles.includes("appimage") !== target.appImageTools) {
      problems.push(`${target.id}: AppImage tool flag does not match bundle set`);
    }
    for (const bundle of target.tauriBundles) {
      if (bundle === "app") {
        if (!target.id.startsWith("macos-") || !target.updaterSignature) {
          problems.push(`${target.id}: internal app bundle is only valid for macOS updater checks`);
        }
      } else if (!target.bundles.includes(bundle)) {
        problems.push(`${target.id}: Tauri bundle ${bundle} is not published`);
      }
    }
    if (target.id.startsWith("macos-") && !target.tauriBundles.includes("app")) {
      problems.push(`${target.id}: macOS updater tripwire requires the internal app bundle`);
    }
    if (target.id.startsWith("macos-") && target.tauriBundles.join(",") !== "app") {
      problems.push(`${target.id}: macOS Tauri bundles must be app-only; DMG uses the bounded builder`);
    }
    if (target.id.startsWith("macos-")) {
      if (!target.notarizationArtifact?.startsWith("dsh-macos-notarization-")) {
        problems.push(`${target.id}: macOS target is missing a private notarization artifact`);
      }
      if (target.notarizationArtifact?.startsWith("deepseek-harness-desktop-")) {
        problems.push(`${target.id}: notarization handoff must not match the public artifact pattern`);
      }
    } else if (target.notarizationArtifact !== undefined) {
      problems.push(`${target.id}: only macOS targets may define a notarization artifact`);
    }
    if (target.updaterSignature) {
      const uploadPath = `${target.updaterSignature.directory}/*${target.updaterSignature.suffix}`;
      if (target.uploadPaths.includes(uploadPath) !== target.updaterSignature.publish) {
        problems.push(`${target.id}: updater signature publication does not match policy`);
      }
      if (target.updaterSignature.publish && !target.updaterSignature.platform) {
        problems.push(`${target.id}: published updater signature needs an exact manifest platform`);
      }
      if (!target.updaterSignature.publish && target.updaterSignature.platform) {
        problems.push(`${target.id}: build-only updater signature must not declare a manifest platform`);
      }
      if (
        target.updaterSignature.platform &&
        !WINDOWS_NSIS_UPDATER_PLATFORMS.includes(target.updaterSignature.platform)
      ) {
        problems.push(`${target.id}: updater manifest platform is not reviewed`);
      }
      if (target.updaterSignature.publish) {
        if (!target.id.startsWith("windows-") || !target.bundles.includes("nsis")) {
          problems.push(`${target.id}: only a Windows NSIS bundle may publish an updater payload`);
        }
        if (
          target.updaterSignature.platform &&
          !updaterPlatforms.add(target.updaterSignature.platform)
        ) {
          problems.push(`${target.id}: duplicate updater manifest platform ${target.updaterSignature.platform}`);
        }
      }
    }
  }

  for (const required of Object.keys(BUNDLE_SPECS) as PublicBundle[]) {
    if (!formats.has(required)) problems.push(`missing public bundle format: ${required}`);
  }
  if (
    publishedWindowsNsisUpdaterPlatforms().join(",") !==
    WINDOWS_NSIS_UPDATER_PLATFORMS.join(",")
  ) {
    problems.push("published updater platforms must exactly match the reviewed Windows NSIS architecture list");
  }
  if (STORE_MSIX_TARGETS.map((target) => target.arch).join(",") !== "x64,arm64") {
    problems.push("Store MSIX matrix must remain exactly x64,arm64");
  }
  for (const target of STORE_MSIX_TARGETS) {
    const nativeTarget = NATIVE_RELEASE_TARGETS.find(
      (candidate) => candidate.id === target.nativeTarget,
    );
    if (!nativeTarget) {
      problems.push(`Store MSIX ${target.arch}: native target ${target.nativeTarget} is missing`);
    } else if (
      nativeTarget.arch !== target.arch ||
      nativeTarget.os !== target.os ||
      nativeTarget.hostTriple !== target.target
    ) {
      problems.push(`Store MSIX ${target.arch}: native host contract drifted from ${target.nativeTarget}`);
    }
  }
  if ([...artifacts].some((name) => name.includes("store-msix"))) {
    problems.push("Store MSIX must not use the public release artifact namespace");
  }
  return problems;
}

/** Exact Tauri manifest keys for public in-app updater payloads. */
export function publishedWindowsNsisUpdaterPlatforms(): readonly WindowsNsisUpdaterPlatform[] {
  return NATIVE_RELEASE_TARGETS.flatMap((target) => {
    const signature = target.updaterSignature;
    return signature?.publish && signature.platform ? [signature.platform] : [];
  });
}

function selectedNativeTargets(selection = "all"): readonly NativeReleaseTarget[] {
  if (selection === "all") return NATIVE_RELEASE_TARGETS;
  const selected = targetById(selection);
  if (!selected) throw new Error(`unknown native release target: ${selection}`);
  return [selected];
}

export function githubNativeMatrix(selection = "all"): { include: Record<string, unknown>[] } {
  return {
    include: selectedNativeTargets(selection).map((target) => ({
      target: target.id,
      os: target.os,
      arch: target.arch,
      hostTriple: target.hostTriple,
      bundles: target.bundles.join(","),
      tauriBundles: target.tauriBundles.join(","),
      artifact: target.artifact,
      paths: target.uploadPaths.join("\n"),
      appImageTools: target.appImageTools,
      flatpak: target.flatpak,
      notarizationArtifact: target.notarizationArtifact,
    })),
  };
}

export function githubMacosNotarizationMatrix(
  selection = "all",
): { include: Record<string, unknown>[] } {
  return {
    include: selectedNativeTargets(selection).filter((target) => target.id.startsWith("macos-")).map(
      (target) => ({
        target: target.id,
        os: target.os,
        arch: target.arch,
        handoffArtifact: target.notarizationArtifact,
        artifact: target.artifact,
        paths: target.uploadPaths.join("\n"),
      }),
    ),
  };
}

export function githubMsixMatrix(): { include: Record<string, unknown>[] } {
  return {
    include: STORE_MSIX_TARGETS.map((target) => ({
      ...target,
      artifact: `dsh-desktop-store-msix-${target.arch}`,
    })),
  };
}

export function targetById(id: string): NativeReleaseTarget | undefined {
  return NATIVE_RELEASE_TARGETS.find((target) => target.id === id);
}

export function bundleArtifactCandidates(
  repoRoot: string,
  bundle: PublicBundle,
  installerLocale?: WindowsWixInstallerLocale,
): string[] {
  const spec = BUNDLE_SPECS[bundle];
  const directory = join(repoRoot, ...spec.directory.split("/"));
  if (!existsSync(directory)) return [];
  return readdirSync(directory)
    .filter((name) => {
      if (!name.endsWith(spec.suffix)) return false;
      return installerLocale === undefined
        ? true
        : bundle === "msi" && wixInstallerLocaleFromMsiName(name) === installerLocale;
    })
    .sort()
    .map((name) => join(directory, name));
}

export function publicArtifactCandidates(
  repoRoot: string,
  artifact: PublicArtifact,
): string[] {
  return bundleArtifactCandidates(repoRoot, artifact.bundle, artifact.installerLocale);
}

export function updaterSignatureCandidates(
  repoRoot: string,
  target: NativeReleaseTarget,
): string[] {
  const signature = target.updaterSignature;
  if (!signature) return [];
  const directory = join(repoRoot, ...signature.directory.split("/"));
  if (!existsSync(directory)) return [];
  return readdirSync(directory)
    .filter((name) => name.endsWith(signature.suffix))
    .sort()
    .map((name) => join(directory, name));
}
