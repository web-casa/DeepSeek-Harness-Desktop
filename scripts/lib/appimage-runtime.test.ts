import assert from "node:assert/strict";
import {
  chmodSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH,
  APPIMAGE_GSTREAMER_SCANNER_RELATIVE_PATH,
  appImageRuntimeProblems,
  bundledHostAbiRuntimeLibraries,
  stripBundledHostAbiRuntimeLibraries,
} from "./appimage-runtime.ts";
import { repoRoot } from "./common.ts";

function fixture(): string {
  const root = mkdtempSync(join(tmpdir(), "dsh-appimage-runtime-test-"));
  mkdirSync(join(root, APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH), { recursive: true });
  writeFileSync(
    join(root, APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH, "libgstapp.so"),
    "fixture",
  );
  const scanner = join(root, APPIMAGE_GSTREAMER_SCANNER_RELATIVE_PATH);
  mkdirSync(join(root, "usr", "lib", "gstreamer1.0", "gstreamer-1.0"), {
    recursive: true,
  });
  writeFileSync(scanner, "fixture");
  chmodSync(scanner, 0o755);
  const main = join(root, "usr", "bin", "deepseek-harness-desktop");
  mkdirSync(join(root, "usr", "bin"), { recursive: true });
  for (const path of [join(root, "AppRun"), join(root, "AppRun.wrapped"), main]) {
    writeFileSync(path, "fixture");
    chmodSync(path, 0o755);
  }
  return root;
}

test("AppImage compatibility policy strips only known host-ABI runtime files", () => {
  const root = fixture();
  try {
    const libraryDirectory = join(root, "usr", "lib");
    writeFileSync(join(libraryDirectory, "libwayland-client.so.0"), "client");
    writeFileSync(join(libraryDirectory, "libwayland-egl.so.1"), "egl");
    writeFileSync(join(libraryDirectory, "libglib-2.0.so.0"), "glib");
    writeFileSync(join(libraryDirectory, "libgio-2.0.so.0"), "gio");
    writeFileSync(join(libraryDirectory, "libgobject-2.0.so.0"), "gobject");
    writeFileSync(join(libraryDirectory, "libgmodule-2.0.so.0"), "gmodule");
    writeFileSync(join(libraryDirectory, "libgthread-2.0.so.0"), "gthread");
    writeFileSync(join(libraryDirectory, "libnghttp2.so.14"), "nghttp2");
    writeFileSync(join(libraryDirectory, "libwayland-scanner"), "keep");
    writeFileSync(join(libraryDirectory, "libglib-private.so.0"), "keep");
    mkdirSync(join(libraryDirectory, "nested"));
    writeFileSync(join(libraryDirectory, "nested", "libwayland-cursor.so.0.1.0"), "cursor");

    assert.deepEqual(
      bundledHostAbiRuntimeLibraries(root).map((path) => path.slice(root.length + 1)),
      [
        "usr/lib/libgio-2.0.so.0",
        "usr/lib/libglib-2.0.so.0",
        "usr/lib/libgmodule-2.0.so.0",
        "usr/lib/libgobject-2.0.so.0",
        "usr/lib/libgthread-2.0.so.0",
        "usr/lib/libnghttp2.so.14",
        "usr/lib/libwayland-client.so.0",
        "usr/lib/libwayland-egl.so.1",
        "usr/lib/nested/libwayland-cursor.so.0.1.0",
      ],
    );
    assert.deepEqual(stripBundledHostAbiRuntimeLibraries(root), [
      "usr/lib/libgio-2.0.so.0",
      "usr/lib/libglib-2.0.so.0",
      "usr/lib/libgmodule-2.0.so.0",
      "usr/lib/libgobject-2.0.so.0",
      "usr/lib/libgthread-2.0.so.0",
      "usr/lib/libnghttp2.so.14",
      "usr/lib/libwayland-client.so.0",
      "usr/lib/libwayland-egl.so.1",
      "usr/lib/nested/libwayland-cursor.so.0.1.0",
    ]);
    assert.deepEqual(bundledHostAbiRuntimeLibraries(root), []);
    assert.equal(readFileSync(join(libraryDirectory, "libwayland-scanner"), "utf8"), "keep");
    assert.equal(readFileSync(join(libraryDirectory, "libglib-private.so.0"), "utf8"), "keep");
    assert.deepEqual(appImageRuntimeProblems(root), []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("AppImage compatibility policy rejects a missing or empty GStreamer plugin directory", () => {
  const root = mkdtempSync(join(tmpdir(), "dsh-appimage-runtime-test-"));
  try {
    assert.match(appImageRuntimeProblems(root).join("\n"), /plugin directory is missing/);
    mkdirSync(join(root, APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH), { recursive: true });
    assert.match(appImageRuntimeProblems(root).join("\n"), /has no libgst/);
    writeFileSync(
      join(root, APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH, "libgstcoreelements.so"),
      "not appsink",
    );
    assert.match(appImageRuntimeProblems(root).join("\n"), /has no libgstapp/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("AppImage compatibility policy requires the bundled executable GStreamer scanner", () => {
  const root = fixture();
  const scanner = join(root, APPIMAGE_GSTREAMER_SCANNER_RELATIVE_PATH);
  try {
    chmodSync(scanner, 0o644);
    assert.match(appImageRuntimeProblems(root).join("\n"), /scanner is not executable/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("AppImage compatibility policy preserves required launcher executability", () => {
  const root = fixture();
  try {
    chmodSync(join(root, "AppRun"), 0o644);
    assert.match(appImageRuntimeProblems(root).join("\n"), /AppRun/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("release builds enable and verify the AppImage media/runtime compatibility path", () => {
  const tauriConfig = JSON.parse(
    readFileSync(join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"),
  ) as { bundle?: { linux?: { appimage?: { bundleMediaFramework?: boolean } } } };
  assert.equal(tauriConfig.bundle?.linux?.appimage?.bundleMediaFramework, true);

  const workflow = readFileSync(join(repoRoot, ".github", "workflows", "release.yml"), "utf8");
  const postprocess = workflow.indexOf("Post-process AppImage runtime compatibility");
  const verify = workflow.indexOf("Verify complete native release set");
  assert.ok(postprocess >= 0 && postprocess < verify, "post-processing must precede release verification");

  const dependencies = readFileSync(
    join(repoRoot, "scripts", "ci", "install-linux-release-deps.sh"),
    "utf8",
  );
  assert.match(dependencies, /\bgstreamer1\.0-plugins-base\b/);
  assert.match(dependencies, /\bgstreamer1\.0-tools\b/);
  assert.match(dependencies, /\bsquashfs-tools\b/);
  const verifier = readFileSync(join(repoRoot, "scripts", "verify-bundle.ts"), "utf8");
  assert.match(verifier, /gst-inspect-1\.0/);
  assert.match(verifier, /libgstapp\\\.so/);
});
