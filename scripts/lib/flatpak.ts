import type { ReleaseArch } from "./release-artifacts.ts";

export const FLATPAK_ID = "com.yeagoo.dsh-desktop";
export const FLATPAK_RUNTIME_VERSION = "49";

export const FLATPAK_FINISH_ARGS = [
  "--share=network",
  "--share=ipc",
  "--socket=wayland",
  "--socket=fallback-x11",
  "--device=dri",
  "--talk-name=org.kde.StatusNotifierWatcher",
  "--filesystem=xdg-run/tray-icon:create",
] as const;

export function flatpakArch(arch: ReleaseArch): "x86_64" | "aarch64" {
  return arch === "x64" ? "x86_64" : "aarch64";
}

export function flatpakManifest(): Record<string, unknown> {
  const desktopFile = `${FLATPAK_ID}.desktop`;
  const metainfoFile = `${FLATPAK_ID}.metainfo.xml`;
  return {
    "app-id": FLATPAK_ID,
    runtime: "org.gnome.Platform",
    "runtime-version": FLATPAK_RUNTIME_VERSION,
    sdk: "org.gnome.Sdk",
    command: "deepseek-harness-desktop",
    branch: "stable",
    "finish-args": FLATPAK_FINISH_ARGS,
    "build-options": {
      strip: false,
      "no-debuginfo": true,
    },
    modules: [
      {
        name: "dsh-desktop",
        buildsystem: "simple",
        "build-commands": [
          "mkdir deb && cd deb && ar x ../app.deb && tar -xf data.tar.*",
          'install -Dm755 "deb/usr/bin/deepseek-harness-desktop" "/app/bin/deepseek-harness-desktop"',
          'mkdir -p "/app/lib/DSH Desktop" && cp -a "deb/usr/lib/DSH Desktop/." "/app/lib/DSH Desktop/"',
          `sed -e 's/^Icon=.*/Icon=${FLATPAK_ID}/' "deb/usr/share/applications/DSH Desktop.desktop" > "${desktopFile}"`,
          `install -Dm644 "${desktopFile}" "/app/share/applications/${desktopFile}"`,
          'mkdir -p /app/share/icons/hicolor && cp -a "deb/usr/share/icons/hicolor/." /app/share/icons/hicolor/',
          `for icon in /app/share/icons/hicolor/*/apps/deepseek-harness-desktop.png; do mv "$icon" "$(dirname "$icon")/${FLATPAK_ID}.png"; done`,
          `install -Dm644 "${metainfoFile}" "/app/share/metainfo/${metainfoFile}"`,
        ],
        sources: [
          { type: "file", path: "app.deb", "dest-filename": "app.deb" },
          {
            type: "file",
            path: metainfoFile,
            "dest-filename": metainfoFile,
          },
        ],
      },
    ],
  };
}

export function flatpakContractProblems(): string[] {
  const problems: string[] = [];
  const finishArgs = FLATPAK_FINISH_ARGS as readonly string[];
  if (finishArgs.includes("--filesystem=host")) {
    problems.push("Flatpak must not receive host filesystem access");
  }
  if (finishArgs.includes("--socket=session-bus")) {
    problems.push("Flatpak must not receive unrestricted session bus access");
  }
  for (const required of [
    "--share=network",
    "--socket=wayland",
    "--socket=fallback-x11",
    "--talk-name=org.kde.StatusNotifierWatcher",
  ]) {
    if (!finishArgs.includes(required)) {
      problems.push(`Flatpak is missing required permission: ${required}`);
    }
  }
  return problems;
}

export function flatpakMetadataProblems(xml: string): string[] {
  const problems: string[] = [];
  if (!xml.includes(`<id>${FLATPAK_ID}</id>`)) {
    problems.push(`Flatpak AppStream metadata must identify ${FLATPAK_ID}`);
  }
  if (
    !xml.includes(
      `<launchable type="desktop-id">${FLATPAK_ID}.desktop</launchable>`,
    )
  ) {
    problems.push(`Flatpak AppStream launchable must be ${FLATPAK_ID}.desktop`);
  }
  if (!xml.includes("<project_license>MIT</project_license>")) {
    problems.push("Flatpak AppStream project license must remain MIT");
  }
  return problems;
}
