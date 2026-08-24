// Reviewed Snap Store packaging contract. Keep this dependency-free so the
// release preflight can validate the source recipe before any package build.

import type { ReleaseArch } from "./release-artifacts.ts";
import { normalizeLineEndings } from "./common.ts";

export const SNAP_NAME = "dsh-desktop-community";
export const SNAP_BASE = "core24";
export const SNAP_TITLE = "DSH Desktop (Community)";
export const SNAP_ICON = "snap/gui/dsh-desktop-community.png";
// `command-chain` is added to the final metadata by Snapcraft when an app
// declares `command-chain`.  Do not declare it twice in the source recipe:
// Snapcraft 9.0.1 would preserve the duplicate in meta/snap.yaml.  Keep the
// source and final contracts separate so both are verified precisely.
export const SNAP_SOURCE_ASSUMES = ["snapd2.43", "common-data-dir"] as const;
export const SNAP_ASSUMES = [...SNAP_SOURCE_ASSUMES, "command-chain"] as const;
export const SNAPCRAFT_VERSION = "9.0.1";
// Snap Store revisions identify one architecture-specific payload, even where
// both payloads report the same Snapcraft version. Keep this map exact.
export const SNAPCRAFT_REVISIONS = {
  amd64: "18514",
  arm64: "18519",
} as const;
export const SNAP_COMMAND_CHAIN = [
  "snap/command-chain/gpu-2404-wrapper",
  "snap/command-chain/desktop-launch",
] as const;
// `GTK_USE_PORTAL=1` is intentional: it keeps file pickers and URI opening
// inside the host's reviewed XDG portal boundary. WebKitGTK/GLib also asks
// the NetworkMonitor portal for connectivity/proxy state; Snap gates that
// read-only request behind `network-status`, separately from network I/O.
// Keep this minimum explicit so a future packaging cleanup cannot reintroduce
// a white/error-only Harness window under strict confinement.
export const SNAP_PORTAL_REQUIRED_APP_PLUGS = ["desktop", "network-status"] as const;
export const SNAP_APP_PLUGS = [
  "network",
  "network-bind",
  "network-status",
  "desktop",
  "desktop-legacy",
  "gsettings",
  "opengl",
  "wayland",
  "x11",
] as const;
export const SNAP_DECLARED_PLUGS = [
  "desktop",
  "gpu-2404",
  "gtk-3-themes",
  "icon-themes",
  "sound-themes",
  "gnome-46-2404",
] as const;

export type SnapArchitecture = "amd64" | "arm64";

export interface SnapReleaseTarget {
  id: "snap-linux-x64" | "snap-linux-arm64";
  os: "ubuntu-24.04" | "ubuntu-24.04-arm";
  arch: ReleaseArch;
  snapArchitecture: SnapArchitecture;
  nativeTarget: "linux-x64" | "linux-arm64";
}

// Each row intentionally uses the same operating system and CPU architecture
// as its bundled Node, sidecar, Tauri shell, and Snap payload.
export const SNAP_RELEASE_TARGETS: readonly SnapReleaseTarget[] = [
  {
    id: "snap-linux-x64",
    os: "ubuntu-24.04",
    arch: "x64",
    snapArchitecture: "amd64",
    nativeTarget: "linux-x64",
  },
  {
    id: "snap-linux-arm64",
    os: "ubuntu-24.04-arm",
    arch: "arm64",
    snapArchitecture: "arm64",
    nativeTarget: "linux-arm64",
  },
] as const;

export function snapTargetForArch(arch: ReleaseArch): SnapReleaseTarget {
  const target = SNAP_RELEASE_TARGETS.find((candidate) => candidate.arch === arch);
  if (!target) throw new Error(`no reviewed Snap target for ${arch}`);
  return target;
}

export function githubSnapMatrix(): { include: SnapReleaseTarget[] } {
  return { include: SNAP_RELEASE_TARGETS.map((target) => ({ ...target })) };
}

export const SNAP_LAUNCHER = `#!/bin/sh
# Snap launcher: make every mutable application path revision-independent.
# Rust still creates and hardens DSH_HOME itself (0700 + no symlink policy).
set -eu

: "\${SNAP:?Snap runtime variable is required}"
: "\${SNAP_USER_COMMON:?Snap common user data directory is required}"

# Never permit a production Snap invocation to redirect the immutable bundled
# runtime into a caller-controlled directory. DSH_RUNTIME_DIR remains a local
# development/test override for non-Snap builds only.
unset DSH_RUNTIME_DIR
export DSH_HOME="$SNAP_USER_COMMON/harness"
export XDG_DATA_HOME="$SNAP_USER_COMMON/xdg-data"
export XDG_CONFIG_HOME="$SNAP_USER_COMMON/xdg-config"
export XDG_CACHE_HOME="$SNAP_USER_COMMON/xdg-cache"

exec "$SNAP/usr/bin/deepseek-harness-desktop" "$@"
`;

export const SNAP_DESKTOP_ENTRY = `[Desktop Entry]
Name=DSH Desktop (Community)
Comment=Community desktop packaging of DeepSeek Harness
Exec=dsh-desktop-community %U
Icon=\${SNAP}/meta/gui/dsh-desktop-community.png
Terminal=false
Type=Application
Categories=Development;
MimeType=x-scheme-handler/dsharness;
StartupNotify=true
StartupWMClass=deepseek-harness-desktop
`;

// These tiny relays are deliberately maintained locally. Snapcraft 9.0.1's
// built-in GNOME extension adds a `gpu/cleanup` part sourced from a mutable
// Git branch. The explicit runtime graph below preserves the extension's
// core24 WebKit/GPU behavior without accepting that build-time Git input.
export const SNAP_GPU_WRAPPER = `#!/bin/bash
# Keep GPU setup inside the signed Store-provided content interface.  This is
# intentionally local code: the Snapcraft GNOME extension currently fetches a
# mutable helper repository during builds, which is not acceptable here.
if [ "$#" -eq 0 ]; then
  echo "DSH Desktop GPU command chain is missing its next step." >&2
  exit 1
fi
if [ -z "\${SNAP:-}" ]; then
  echo "DSH Desktop is not running inside a Snap context." >&2
  exit 2
fi

provider="\${SNAP}/gpu-2404/bin/gpu-2404-provider-wrapper"
if [ ! -f "$provider" ]; then
  echo "DSH Desktop requires the connected mesa-2404 GPU content provider." >&2
  exit 3
fi

# The provider prepares GPU paths before continuing into the next command-chain
# entry. Source it with the same Bash $0 hand-off used by Snapcraft 9.0.1's
# reviewed GPU relay. Do not enable errexit/nounset here: the signed provider
# owns its shell options and uses \`exec "$@"\` to continue the chain.
BASH_ARGV0="$provider"
# shellcheck source=/dev/null
source "$provider"
`;

export const SNAP_COMMAND_CHAIN_RUNNER = `#!/bin/bash
# Source the next trusted command-chain step so it can prepare environment
# variables for the final application. The next path is supplied by snapd from
# the reviewed command-chain metadata, never by the application URI argument.
if [ "$#" -eq 0 ]; then
  echo "DSH Desktop command chain is missing its next step." >&2
  exit 1
fi
if [ -z "\${SNAP:-}" ]; then
  echo "DSH Desktop is not running inside a Snap context." >&2
  exit 2
fi
if [ ! -f "$1" ]; then
  echo "DSH Desktop command-chain step is unavailable: $1" >&2
  exit 3
fi

# Match Snapcraft's standard relay exactly: BASH_ARGV0 changes Bash's $0 for
# the sourced content-provider script after the executable path is removed
# from its argument vector. In particular, do not inherit \`set -u\` into the
# gnome-46 launcher: its first-run state variable is intentionally optional.
BASH_ARGV0="$1"
shift
# shellcheck source=/dev/null
source "$0"
`;

export const SNAP_DESKTOP_LAUNCHER = `#!/bin/bash
# Delegate desktop environment setup to the signed gnome-46-2404 content snap.
# The local relay keeps the build recipe free of mutable extension Git sources.
if [ -z "\${SNAP:-}" ]; then
  echo "DSH Desktop is not running inside a Snap context." >&2
  exit 2
fi

launcher="\${SNAP}/gnome-platform/command-chain/desktop-launch"
runner="\${SNAP}/snap/command-chain/run"
if [ ! -f "$launcher" ] || [ ! -f "$runner" ]; then
  echo "DSH Desktop requires the connected gnome-46-2404 platform content snap." >&2
  exit 3
fi

# Do not set errexit or nounset in this relay. The signed desktop launcher
# deliberately reads optional first-run state before it initializes it.
set -- "$launcher" "$@"
# shellcheck source=/dev/null
source "$runner"
`;

export function reviewedSnapRecipe(version: string): string {
  return `name: ${SNAP_NAME}
base: ${SNAP_BASE}
version: "${version}"
title: ${SNAP_TITLE}
summary: Community desktop packaging of DeepSeek Harness
description: |
  DSH Desktop packages the official DeepSeek Harness with a pinned Node runtime
  in a native, community-maintained desktop shell. It is not an upstream
  DeepSeek product and does not modify the Harness Web UI.
grade: stable
confinement: strict
license: MIT
icon: ${SNAP_ICON}
website: https://dsharness.app
source-code: https://github.com/web-casa/DeepSeek-Harness-Desktop
issues: https://github.com/web-casa/DeepSeek-Harness-Desktop/issues

# Do not use \`extensions: [gnome]\` here. In the reviewed Snapcraft ${SNAPCRAFT_VERSION} it
# implicitly clones canonical/gpu-snap from a mutable branch for a cleanup
# workaround. This explicit, source-free equivalent uses only signed Store
# content providers at runtime and local command-chain relays in this repo.
assumes:
${SNAP_SOURCE_ASSUMES.map((assumption) => `  - ${assumption}`).join("\n")}

plugs:
  desktop:
    mount-host-font-cache: false
  gpu-2404:
    interface: content
    target: $SNAP/gpu-2404
    default-provider: mesa-2404
  gtk-3-themes:
    interface: content
    target: $SNAP/data-dir/themes
    default-provider: gtk-common-themes
  icon-themes:
    interface: content
    target: $SNAP/data-dir/icons
    default-provider: gtk-common-themes
  sound-themes:
    interface: content
    target: $SNAP/data-dir/sounds
    default-provider: gtk-common-themes
  gnome-46-2404:
    interface: content
    target: $SNAP/gnome-platform
    default-provider: gnome-46-2404

layout:
  /usr/share/libdrm:
    bind: $SNAP/gpu-2404/libdrm
  /usr/share/drirc.d:
    symlink: $SNAP/gpu-2404/drirc.d
  /usr/share/X11/XErrorDB:
    symlink: $SNAP/gpu-2404/X11/XErrorDB
  /usr/lib/$CRAFT_ARCH_TRIPLET_BUILD_FOR/webkit2gtk-4.0:
    bind: $SNAP/gnome-platform/usr/lib/$CRAFT_ARCH_TRIPLET_BUILD_FOR/webkit2gtk-4.0
  /usr/lib/$CRAFT_ARCH_TRIPLET_BUILD_FOR/webkit2gtk-4.1:
    bind: $SNAP/gnome-platform/usr/lib/$CRAFT_ARCH_TRIPLET_BUILD_FOR/webkit2gtk-4.1
  /usr/lib/$CRAFT_ARCH_TRIPLET_BUILD_FOR/libproxy:
    bind: $SNAP/gnome-platform/usr/lib/$CRAFT_ARCH_TRIPLET_BUILD_FOR/libproxy
  /usr/share/xml/iso-codes:
    bind: $SNAP/gnome-platform/usr/share/xml/iso-codes

environment:
  SNAP_DESKTOP_RUNTIME: $SNAP/gnome-platform
  GTK_USE_PORTAL: "1"

# Snapcraft builds each platform only on a runner of the same native CPU
# architecture. The workflow independently asserts the Node and Rust host
# triples before staging the source-built Debian payload.
platforms:
  amd64:
    build-on: [amd64]
    build-for: [amd64]
  arm64:
    build-on: [arm64]
    build-for: [arm64]

apps:
  ${SNAP_NAME}:
    command: bin/launch-dsh-desktop
    command-chain:
      - ${SNAP_COMMAND_CHAIN[0]}
      - ${SNAP_COMMAND_CHAIN[1]}
    plugs:
${SNAP_APP_PLUGS.map((plug) => `      - ${plug}`).join("\n")}

# The input is a local DEB built from this exact checkout in the same native
# CI job. It is deliberately not a downloaded GitHub Release asset.
parts:
  # The app command is deliberately a checked-in launcher rather than a
  # generated wrapper: it fixes mutable paths to SNAP_USER_COMMON before the
  # immutable DEB payload starts. Stage it explicitly; an apps command
  # declaration alone does not copy a source file into the final Snap.
  dsh-desktop-launcher:
    plugin: dump
    source: snap/bin
    organize:
      launch-dsh-desktop: bin/launch-dsh-desktop
    stage:
      - bin/launch-dsh-desktop
    prime:
      - bin/launch-dsh-desktop
  # Explicitly stage only the three local command-chain scripts. Do not pull a Git
  # helper at build time: all runtime providers are Store-signed content snaps.
  dsh-desktop-command-chain:
    plugin: dump
    source: snap/command-chain
    organize:
      gpu-2404-wrapper: snap/command-chain/gpu-2404-wrapper
      desktop-launch: snap/command-chain/desktop-launch
      run: snap/command-chain/run
    # A newly added source file must not silently enter the signed package.
    stage:
      - snap/command-chain/gpu-2404-wrapper
      - snap/command-chain/desktop-launch
      - snap/command-chain/run
    prime:
      - snap/command-chain/gpu-2404-wrapper
      - snap/command-chain/desktop-launch
      - snap/command-chain/run
  dsh-desktop:
    plugin: dump
    source: target/snap/input/dsh-desktop.deb
    source-type: deb
`;
}

function hasExactLine(text: string, line: string): boolean {
  return new RegExp(`^${line.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`, "m").test(text);
}

export function snapRecipeVersion(recipe: string): string | undefined {
  return /^version: "([^"\r\n]+)"$/m.exec(normalizeLineEndings(recipe))?.[1];
}

export interface SnapDefinitionInput {
  recipe: string;
  launcher: string;
  desktopEntry: string;
  gpuWrapper: string;
  desktopLauncher: string;
  commandChainRunner: string;
  expectedVersion: string;
}

export function snapDefinitionProblems(input: SnapDefinitionInput): string[] {
  const recipe = normalizeLineEndings(input.recipe);
  const launcher = normalizeLineEndings(input.launcher);
  const desktopEntry = normalizeLineEndings(input.desktopEntry);
  const gpuWrapper = normalizeLineEndings(input.gpuWrapper);
  const desktopLauncher = normalizeLineEndings(input.desktopLauncher);
  const commandChainRunner = normalizeLineEndings(input.commandChainRunner);
  const problems: string[] = [];

  for (const [field, expected] of [
    ["name", SNAP_NAME],
    ["base", SNAP_BASE],
    ["title", SNAP_TITLE],
    ["grade", "stable"],
    ["confinement", "strict"],
  ] as const) {
    if (!hasExactLine(recipe, `${field}: ${expected}`)) {
      problems.push(`snap recipe must declare ${field}: ${expected}`);
    }
  }
  if (snapRecipeVersion(recipe) !== input.expectedVersion) {
    problems.push(
      `snap recipe version ${snapRecipeVersion(recipe) ?? "missing"} != expected ${input.expectedVersion}`,
    );
  }
  if (!hasExactLine(recipe, `icon: ${SNAP_ICON}`)) {
    problems.push(`snap recipe must declare icon: ${SNAP_ICON}`);
  }
  if (/^confinement:\s*(?:classic|devmode)\s*$/m.test(recipe)) {
    problems.push("snap recipe must not permit classic or devmode confinement");
  }
  if (/^\s*-\s*(?:home|removable-media)\s*$/m.test(recipe)) {
    problems.push("snap recipe must not request home or removable-media access");
  }
  for (const plug of SNAP_PORTAL_REQUIRED_APP_PLUGS) {
    if (!hasExactLine(recipe, `      - ${plug}`)) {
      problems.push(`snap recipe must enable ${plug} for the GTK/XDG portal runtime`);
    }
  }

  if (/^\s*extensions:/m.test(recipe)) {
    problems.push("snap recipe must not use an extension with implicit build-time sources");
  }
  if (/^\s*source:\s*(?:https?:|git@|ssh:)/m.test(recipe)) {
    problems.push("snap recipe must not download mutable remote part sources");
  }
  if (recipe !== reviewedSnapRecipe(input.expectedVersion)) {
    problems.push("snap recipe diverges from the reviewed source-free GNOME/GPU runtime contract");
  }

  if (launcher !== SNAP_LAUNCHER) {
    problems.push("Snap launcher diverges from the reviewed persistent-data/runtime contract");
  }
  if (desktopEntry !== SNAP_DESKTOP_ENTRY) {
    problems.push("Snap desktop entry diverges from the reviewed URI/launcher contract");
  }
  if (gpuWrapper !== SNAP_GPU_WRAPPER) {
    problems.push("Snap GPU command-chain relay diverges from the reviewed provider contract");
  }
  if (desktopLauncher !== SNAP_DESKTOP_LAUNCHER) {
    problems.push("Snap desktop command-chain relay diverges from the reviewed provider contract");
  }
  if (commandChainRunner !== SNAP_COMMAND_CHAIN_RUNNER) {
    problems.push("Snap command-chain runner diverges from the reviewed provider contract");
  }
  return problems;
}
