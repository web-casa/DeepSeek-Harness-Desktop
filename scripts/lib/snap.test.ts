import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  SNAP_DESKTOP_ENTRY,
  SNAP_DESKTOP_LAUNCHER,
  SNAP_COMMAND_CHAIN_RUNNER,
  SNAP_ASSUMES,
  SNAP_SOURCE_ASSUMES,
  SNAPCRAFT_REVISIONS,
  SNAP_GPU_WRAPPER,
  SNAP_ICON,
  SNAP_LAUNCHER,
  SNAP_NAME,
  SNAP_APP_PLUGS,
  SNAP_PORTAL_REQUIRED_APP_PLUGS,
  SNAP_TITLE,
  SNAP_RELEASE_TARGETS,
  githubSnapMatrix,
  snapDefinitionProblems,
  snapRecipeVersion,
} from "./snap.ts";

const recipe = readFileSync(new URL("../../snap/snapcraft.yaml", import.meta.url), "utf8");
const launcher = readFileSync(new URL("../../snap/bin/launch-dsh-desktop", import.meta.url), "utf8");
const desktopEntry = readFileSync(
  new URL("../../snap/gui/dsh-desktop-community.desktop", import.meta.url),
  "utf8",
);
const gpuWrapper = readFileSync(new URL("../../snap/command-chain/gpu-2404-wrapper", import.meta.url), "utf8");
const desktopLauncher = readFileSync(new URL("../../snap/command-chain/desktop-launch", import.meta.url), "utf8");
const commandChainRunner = readFileSync(new URL("../../snap/command-chain/run", import.meta.url), "utf8");
const buildInfo = readFileSync(new URL("../../src-tauri/src/build_info.rs", import.meta.url), "utf8");

test("reviewed Snap definition is strict, native, and version-aligned", () => {
  assert.equal(snapRecipeVersion(recipe), "0.2.18");
  assert.deepEqual(
    snapDefinitionProblems({
      recipe,
      launcher,
      desktopEntry,
      gpuWrapper,
      desktopLauncher,
      commandChainRunner,
      expectedVersion: "0.2.18",
    }),
    [],
  );
  assert.match(
    recipe,
    /dsh-desktop-launcher:\n    plugin: dump\n    source: snap\/bin\n    organize:\n      launch-dsh-desktop: bin\/launch-dsh-desktop/,
  );
  assert.match(recipe, /stage:\n      - bin\/launch-dsh-desktop\n    prime:\n      - bin\/launch-dsh-desktop/);
});

test("Snap definition rejects weakened confinement, broad filesystem access, and launcher drift", () => {
  assert.ok(
    snapDefinitionProblems({
      recipe: recipe.replace("confinement: strict", "confinement: classic\n    - home"),
      launcher,
      desktopEntry,
      gpuWrapper,
      desktopLauncher,
      commandChainRunner,
      expectedVersion: "0.2.18",
    }).some((problem) => problem.includes("classic or devmode")),
  );
  assert.ok(
    snapDefinitionProblems({
      recipe,
      launcher: launcher.replace("unset DSH_RUNTIME_DIR", "# DSH_RUNTIME_DIR inherited"),
      desktopEntry,
      gpuWrapper,
      desktopLauncher,
      commandChainRunner,
      expectedVersion: "0.2.18",
    }).some((problem) => problem.includes("launcher diverges")),
  );
  assert.ok(
    snapDefinitionProblems({
      recipe,
      launcher,
      desktopEntry: desktopEntry.replace("dsharness;", "other;"),
      gpuWrapper,
      desktopLauncher,
      commandChainRunner,
      expectedVersion: "0.2.18",
    }).some((problem) => problem.includes("desktop entry diverges")),
  );
  assert.ok(
    snapDefinitionProblems({
      recipe,
      launcher,
      desktopEntry,
      gpuWrapper: gpuWrapper.replace("gpu-2404-provider-wrapper", "untrusted-wrapper"),
      desktopLauncher,
      commandChainRunner,
      expectedVersion: "0.2.18",
    }).some((problem) => problem.includes("GPU command-chain")),
  );
  assert.ok(
    snapDefinitionProblems({
      recipe: recipe.replace("source: snap/command-chain", "source: https://example.invalid/repo.git"),
      launcher,
      desktopEntry,
      gpuWrapper,
      desktopLauncher,
      commandChainRunner,
      expectedVersion: "0.2.18",
    }).some((problem) => problem.includes("must not download mutable")),
  );
  assert.ok(
    snapDefinitionProblems({
      recipe: recipe.replace("      - network-status\n", ""),
      launcher,
      desktopEntry,
      gpuWrapper,
      desktopLauncher,
      commandChainRunner,
      expectedVersion: "0.2.18",
    }).some((problem) => problem.includes("network-status for the GTK/XDG portal runtime")),
  );
});

test("Snap matrix is exactly native amd64 and arm64", () => {
  assert.equal(SNAP_NAME, "dsh-desktop-community");
  assert.equal(SNAP_TITLE, "DSH Desktop (Community)");
  assert.equal(SNAP_ICON, "snap/gui/dsh-desktop-community.png");
  assert.deepEqual(SNAP_PORTAL_REQUIRED_APP_PLUGS, ["desktop", "network-status"]);
  assert.deepEqual(SNAP_APP_PLUGS, [
    "network",
    "network-bind",
    "network-status",
    "desktop",
    "desktop-legacy",
    "gsettings",
    "opengl",
    "wayland",
    "x11",
  ]);
  assert.deepEqual(SNAPCRAFT_REVISIONS, { amd64: "18514", arm64: "18519" });
  assert.deepEqual(SNAP_SOURCE_ASSUMES, ["snapd2.43", "common-data-dir"]);
  assert.deepEqual(SNAP_ASSUMES, ["snapd2.43", "common-data-dir", "command-chain"]);
  assert.deepEqual(
    SNAP_RELEASE_TARGETS.map((target) => [target.os, target.arch, target.snapArchitecture, target.nativeTarget]),
    [
      ["ubuntu-24.04", "x64", "amd64", "linux-x64"],
      ["ubuntu-24.04-arm", "arm64", "arm64", "linux-arm64"],
    ],
  );
  assert.deepEqual(githubSnapMatrix().include, SNAP_RELEASE_TARGETS);
  assert.match(SNAP_LAUNCHER, /DSH_HOME="\$SNAP_USER_COMMON\/harness"/);
  assert.match(SNAP_DESKTOP_ENTRY, /Exec=dsh-desktop-community %U/);
  assert.match(SNAP_GPU_WRAPPER, /gpu-2404-provider-wrapper/);
  assert.match(SNAP_DESKTOP_LAUNCHER, /gnome-platform\/command-chain\/desktop-launch/);
  assert.match(SNAP_COMMAND_CHAIN_RUNNER, /source "\$0"/);
  assert.match(buildInfo, new RegExp(`const SNAP_NAME: &str = "${SNAP_NAME}"`));
});
