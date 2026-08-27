import type { ReleaseArch } from "./release-artifacts.ts";

export interface AppImageTool {
  cacheName: string;
  source: string;
  sha256: string;
}

// Tauri's follow-up to the Wayland AppImage regression keeps X11 as the
// compatibility fallback, while allowing a user to explicitly select a GTK
// backend before launching the AppImage.
export const APPIMAGE_GTK_HOOK_RELATIVE_PATH =
  "apprun-hooks/linuxdeploy-plugin-gtk.sh";
export const APPIMAGE_GDK_BACKEND_EXPORT = 'export GDK_BACKEND="${GDK_BACKEND:-x11}"';

const sharedTools = [
  {
    cacheName: "linuxdeploy-plugin-gtk.sh",
    source:
      "https://raw.githubusercontent.com/tauri-apps/tauri/7164de39574d616b762ba658f797f9657ea03b20/crates/tauri-bundler/src/bundle/linux/appimage/linuxdeploy-plugin-gtk.sh",
    sha256: "fe83c123e65977752f83b347d0936d59d03dabe883141b208b04b2544ebf108d",
  },
  {
    cacheName: "linuxdeploy-plugin-gstreamer.sh",
    source:
      "https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gstreamer/2a2e67491c32995a3f279ad0ecbe77abd512b42a/linuxdeploy-plugin-gstreamer.sh",
    sha256: "c107b49d84edbffc6ab226ed1007e0626a4f7aa2c3a36b7782bef62351d49e94",
  },
] as const satisfies readonly AppImageTool[];

const architectureTools: Readonly<Record<ReleaseArch, readonly AppImageTool[]>> = {
  arm64: [
    {
      cacheName: "AppRun-aarch64",
      source:
        "https://api.github.com/repos/tauri-apps/binary-releases/releases/assets/274691716",
      sha256: "072f17c0895a85c490282fe5395c5007e5fc75da727e553b3b8fb680feb11578",
    },
    {
      cacheName: "linuxdeploy-aarch64.AppImage",
      source:
        "https://api.github.com/repos/tauri-apps/binary-releases/releases/assets/181589153",
      sha256: "b12b5cc57bd0921e1f98d73f58aa364503bc1a27f54b7a69fd2870bce7fa2f55",
    },
    {
      cacheName: "linuxdeploy-plugin-appimage.AppImage",
      source:
        "https://api.github.com/repos/linuxdeploy/linuxdeploy-plugin-appimage/releases/assets/497460673",
      sha256: "6fdecf5bf8af4e0db03c6b2a80976acc3c96b6a4d19622fa6c6adfd308378bbc",
    },
  ],
  x64: [
    {
      cacheName: "AppRun-x86_64",
      source:
        "https://api.github.com/repos/tauri-apps/binary-releases/releases/assets/274691722",
      sha256: "f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f",
    },
    {
      cacheName: "linuxdeploy-x86_64.AppImage",
      source:
        "https://api.github.com/repos/tauri-apps/binary-releases/releases/assets/182515537",
      sha256: "e762bea85c8eb0d4b3508d46e5c1f037f717d0f9303ae3b4aafc8b04991fa1ef",
    },
    {
      cacheName: "linuxdeploy-plugin-appimage.AppImage",
      source:
        "https://api.github.com/repos/linuxdeploy/linuxdeploy-plugin-appimage/releases/assets/497460911",
      sha256: "a45d3e227bc7f397e9cf6bfa4c9507494efa2293357b6e86690a3de2ca992e79",
    },
  ],
};

export function appImageToolsForArch(arch: ReleaseArch): readonly AppImageTool[] {
  return [...architectureTools[arch], ...sharedTools];
}

export function appImageToolDefinitionProblems(arch: ReleaseArch): string[] {
  const problems: string[] = [];
  const names = new Set<string>();
  for (const tool of appImageToolsForArch(arch)) {
    if (!/^[A-Za-z0-9._-]+$/.test(tool.cacheName)) {
      problems.push(`unsafe AppImage cache filename: ${tool.cacheName}`);
    }
    if (names.has(tool.cacheName)) {
      problems.push(`duplicate AppImage cache filename: ${tool.cacheName}`);
    }
    names.add(tool.cacheName);
    if (!/^[a-f0-9]{64}$/.test(tool.sha256)) {
      problems.push(`invalid SHA-256 for ${tool.cacheName}`);
    }
    const source = new URL(tool.source);
    const isAssetApi =
      source.origin === "https://api.github.com" &&
      /^\/repos\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/releases\/assets\/[1-9][0-9]*$/.test(
        source.pathname,
      );
    const isRawCommit =
      source.origin === "https://raw.githubusercontent.com" &&
      /^\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/[a-f0-9]{40}\/[A-Za-z0-9._/-]+$/.test(
        source.pathname,
      );
    if (!isAssetApi && !isRawCommit) {
      problems.push(`AppImage source is not pinned by asset ID or commit: ${tool.source}`);
    }
  }
  if (names.size !== 5) {
    problems.push(`${arch} AppImage tool set must contain exactly five files`);
  }
  return problems;
}
