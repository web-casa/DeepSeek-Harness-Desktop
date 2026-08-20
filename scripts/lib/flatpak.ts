import type { ReleaseArch } from "./release-artifacts.ts";

export const FLATPAK_ID = "com.yeagoo.dsh-desktop";
export const FLATPAK_RUNTIME_VERSION = "49";
export const FLATPAK_BRANCH = "stable";
export const FLATPAK_COMMAND = "deepseek-harness-desktop";
export const FLATPAK_RUNTIME_REPO = "https://dl.flathub.org/repo/flathub.flatpakrepo";

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

export function flatpakContractProblems(): string[] {
  const problems: string[] = [];
  if (FLATPAK_RUNTIME_REPO !== "https://dl.flathub.org/repo/flathub.flatpakrepo") {
    problems.push("Flatpak runtime repository must remain the reviewed Flathub HTTPS endpoint");
  }
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
