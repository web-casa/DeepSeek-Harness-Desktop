import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";

export type ReleaseArch = "x64" | "arm64";
export type PublicBundle =
  | "nsis"
  | "msi"
  | "dmg"
  | "appimage"
  | "deb"
  | "rpm"
  | "flatpak";

// `app` is an internal macOS bundle target used only to materialize the
// signed updater tripwire. It is never a public release asset; users receive
// the DMG.
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
  bundles: readonly PublicBundle[];
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
  };
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
    bundles: ["nsis", "msi"],
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
    },
  },
  {
    id: "macos-arm64",
    os: "macos-15",
    arch: "arm64",
    bundles: ["dmg"],
    tauriBundles: ["dmg", "app"],
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
    bundles: ["dmg"],
    tauriBundles: ["dmg", "app"],
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
    bundles: ["appimage", "deb", "rpm", "flatpak"],
    tauriBundles: ["appimage", "deb", "rpm"],
    artifact: "deepseek-harness-desktop-linux-arm64",
    uploadPaths: artifactPaths(["appimage", "deb", "rpm", "flatpak"]),
    appImageTools: true,
    flatpak: true,
  },
] as const;

export const STORE_MSIX_TARGETS = [
  { arch: "x64" as const, os: "windows-latest", target: "x86_64-pc-windows-msvc" },
  { arch: "arm64" as const, os: "windows-11-arm", target: "aarch64-pc-windows-msvc" },
] as const;

export function releasePlanProblems(): string[] {
  const problems: string[] = [];
  const ids = new Set<string>();
  const artifacts = new Set<string>();
  const formats = new Set<PublicBundle>();

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
    }
  }

  for (const required of Object.keys(BUNDLE_SPECS) as PublicBundle[]) {
    if (!formats.has(required)) problems.push(`missing public bundle format: ${required}`);
  }
  if (STORE_MSIX_TARGETS.map((target) => target.arch).join(",") !== "x64,arm64") {
    problems.push("Store MSIX matrix must remain exactly x64,arm64");
  }
  if ([...artifacts].some((name) => name.includes("store-msix"))) {
    problems.push("Store MSIX must not use the public release artifact namespace");
  }
  return problems;
}

export function githubNativeMatrix(): { include: Record<string, unknown>[] } {
  return {
    include: NATIVE_RELEASE_TARGETS.map((target) => ({
      target: target.id,
      os: target.os,
      arch: target.arch,
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

export function githubMacosNotarizationMatrix(): { include: Record<string, unknown>[] } {
  return {
    include: NATIVE_RELEASE_TARGETS.filter((target) => target.id.startsWith("macos-")).map(
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
): string[] {
  const spec = BUNDLE_SPECS[bundle];
  const directory = join(repoRoot, ...spec.directory.split("/"));
  if (!existsSync(directory)) return [];
  return readdirSync(directory)
    .filter((name) => name.endsWith(spec.suffix))
    .sort()
    .map((name) => join(directory, name));
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
