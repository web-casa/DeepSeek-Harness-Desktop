import { existsSync, lstatSync, readdirSync, rmSync } from "node:fs";
import { join, relative } from "node:path";

export const APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH = join(
  "usr",
  "lib",
  "gstreamer-1.0",
);
// The reviewed linuxdeploy GStreamer hook exports this exact scanner path.
// It must be present: without it GStreamer silently falls back to a host
// helper, which can reintroduce a mixed bundle/host plugin process.
export const APPIMAGE_GSTREAMER_SCANNER_RELATIVE_PATH = join(
  "usr",
  "lib",
  "gstreamer1.0",
  "gstreamer-1.0",
  "gst-plugin-scanner",
);

// These libraries have a stable SONAME but are coupled to the target desktop's
// graphics stack or dynamically loaded GIO modules.  Shipping the Ubuntu 22.04
// copies alongside a newer host can create one process containing an old GLib
// or nghttp2 and a newer system module (dconf/libproxy/libcurl).  That is the
// exact ABI split observed on fresh Kubuntu Wayland installs.  Resolve this
// deliberately small, evidence-backed family from the host instead.
const HOST_ABI_RUNTIME_LIBRARY =
  /^(?:libwayland-(?:client|cursor|egl|server)|lib(?:gio|glib|gmodule|gobject|gthread)-2\.0|libnghttp2)\.so(?:\.\d+)*$/;
// WebKit's rendering path needs the `appsink` element specifically. A random
// GStreamer plugin directory is not enough to prevent the observed startup
// failure, so assert the plugin that provides it.
const GSTREAMER_APPSINK_PLUGIN = /^libgstapp\.so(?:\.\d+)*$/;
const REQUIRED_APPIMAGE_EXECUTABLES = [
  "AppRun",
  "AppRun.wrapped",
  join("usr", "bin", "deepseek-harness-desktop"),
] as const;

function bundledLibraryDirectory(root: string): string {
  return join(root, "usr", "lib");
}

/**
 * List the desktop ABI libraries that must be resolved from the host rather
 * than from the AppImage. Do not follow symlinked directories while walking:
 * the extracted image is input to a release-time mutator, so its traversal
 * remains confined to `root` even if a malformed artifact is supplied.
 */
export function bundledHostAbiRuntimeLibraries(root: string): string[] {
  const libDirectory = bundledLibraryDirectory(root);
  if (!existsSync(libDirectory) || !lstatSync(libDirectory).isDirectory()) return [];

  const libraries: string[] = [];
  const walk = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        walk(path);
      } else if (
        (entry.isFile() || entry.isSymbolicLink()) &&
        HOST_ABI_RUNTIME_LIBRARY.test(entry.name)
      ) {
        libraries.push(path);
      }
    }
  };
  walk(libDirectory);
  return libraries.sort();
}

/** Remove only the known host-coupled desktop ABI runtime files. */
export function stripBundledHostAbiRuntimeLibraries(root: string): string[] {
  const libraries = bundledHostAbiRuntimeLibraries(root);
  for (const library of libraries) rmSync(library, { force: true });
  return libraries.map((library) => relative(root, library));
}

/**
 * Checks the post-processing contract, independently of the AppImage
 * container format. It is deliberately strict: the generated AppRun always
 * exports this GStreamer directory and scanner, so missing either recreates
 * the WebKit startup failure this compatibility pass is meant to prevent.
 */
export function appImageRuntimeProblems(root: string): string[] {
  const problems: string[] = [];
  for (const relativePath of REQUIRED_APPIMAGE_EXECUTABLES) {
    const path = join(root, relativePath);
    if (!existsSync(path) || !lstatSync(path).isFile()) {
      problems.push(`required AppImage executable is missing: ${relativePath}`);
    } else if ((lstatSync(path).mode & 0o111) === 0) {
      problems.push(`required AppImage executable is not executable: ${relativePath}`);
    }
  }

  const hostAbiLibraries = bundledHostAbiRuntimeLibraries(root);
  if (hostAbiLibraries.length > 0) {
    problems.push(
      `bundled host-ABI runtime libraries remain: ${hostAbiLibraries
        .map((path) => relative(root, path))
        .join(", ")}`,
    );
  }

  const gstreamerDirectory = join(root, APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH);
  if (!existsSync(gstreamerDirectory) || !lstatSync(gstreamerDirectory).isDirectory()) {
    problems.push(
      `bundled GStreamer plugin directory is missing: ${APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH}`,
    );
    return problems;
  }
  const hasAppsinkPlugin = readdirSync(gstreamerDirectory, { withFileTypes: true }).some(
    (entry) => entry.isFile() && GSTREAMER_APPSINK_PLUGIN.test(entry.name),
  );
  if (!hasAppsinkPlugin) {
    problems.push(
      `bundled GStreamer plugin directory has no libgstapp.so appsink plugin: ${APPIMAGE_GSTREAMER_PLUGIN_RELATIVE_PATH}`,
    );
  }
  const scanner = join(root, APPIMAGE_GSTREAMER_SCANNER_RELATIVE_PATH);
  if (!existsSync(scanner) || !lstatSync(scanner).isFile()) {
    problems.push(
      `bundled GStreamer plugin scanner is missing: ${APPIMAGE_GSTREAMER_SCANNER_RELATIVE_PATH}`,
    );
  } else if ((lstatSync(scanner).mode & 0o111) === 0) {
    problems.push(
      `bundled GStreamer plugin scanner is not executable: ${APPIMAGE_GSTREAMER_SCANNER_RELATIVE_PATH}`,
    );
  }
  return problems;
}
