import { test } from "node:test";
import assert from "node:assert/strict";
import {
  APPIMAGE_GDK_BACKEND_EXPORT,
  APPIMAGE_GTK_HOOK_RELATIVE_PATH,
  appImageToolDefinitionProblems,
  appImageToolsForArch,
} from "./appimage-tools.ts";

for (const arch of ["x64", "arm64"] as const) {
  test(`${arch} AppImage tool sources are immutable and SHA-256 pinned`, () => {
    assert.deepEqual(appImageToolDefinitionProblems(arch), []);
    assert.equal(appImageToolsForArch(arch).length, 5);
  });
}

test("architecture-specific tools do not accidentally share binary hashes", () => {
  const x64 = appImageToolsForArch("x64");
  const arm64 = appImageToolsForArch("arm64");
  const x64BinaryHashes = x64.slice(0, 3).map((tool) => tool.sha256);
  const arm64BinaryHashes = new Set(
    arm64.slice(0, 3).map((tool) => tool.sha256),
  );
  assert.equal(x64BinaryHashes.some((hash) => arm64BinaryHashes.has(hash)), false);
  assert.deepEqual(x64.slice(3), arm64.slice(3));
});

test("GTK AppImage hook preserves an explicitly selected backend", () => {
  const gtkHook = appImageToolsForArch("x64").find(
    (tool) => tool.cacheName === "linuxdeploy-plugin-gtk.sh",
  );
  assert.deepEqual(gtkHook, {
    cacheName: "linuxdeploy-plugin-gtk.sh",
    source:
      "https://raw.githubusercontent.com/tauri-apps/tauri/7164de39574d616b762ba658f797f9657ea03b20/crates/tauri-bundler/src/bundle/linux/appimage/linuxdeploy-plugin-gtk.sh",
    sha256: "fe83c123e65977752f83b347d0936d59d03dabe883141b208b04b2544ebf108d",
  });
  assert.equal(APPIMAGE_GTK_HOOK_RELATIVE_PATH, "apprun-hooks/linuxdeploy-plugin-gtk.sh");
  assert.equal(APPIMAGE_GDK_BACKEND_EXPORT, 'export GDK_BACKEND="${GDK_BACKEND:-x11}"');
});
