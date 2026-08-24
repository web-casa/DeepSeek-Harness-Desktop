import assert from "node:assert/strict";
import test from "node:test";
import {
  SNAP_PROVENANCE_SCHEMA,
  snapArtifactName,
  snapMetadataProblems,
  snapProvenanceProblems,
} from "./snap-package.ts";

const metadata = `name: dsh-desktop-community
title: DSH Desktop (Community)
version: 0.2.16
summary: DSH Desktop
base: core24
grade: stable
confinement: strict
assumes:
- command-chain
- common-data-dir
- snapd2.43
architectures:
  - arm64
apps:
  dsh-desktop-community:
    command: bin/launch-dsh-desktop
    plugs:
    - network
    - network-bind
    - desktop
    - desktop-legacy
    - gsettings
    - opengl
    - wayland
    - x11
    command-chain:
    - snap/command-chain/gpu-2404-wrapper
    - snap/command-chain/desktop-launch
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
  /usr/lib/aarch64-linux-gnu/webkit2gtk-4.0:
    bind: $SNAP/gnome-platform/usr/lib/aarch64-linux-gnu/webkit2gtk-4.0
  /usr/lib/aarch64-linux-gnu/webkit2gtk-4.1:
    bind: $SNAP/gnome-platform/usr/lib/aarch64-linux-gnu/webkit2gtk-4.1
environment:
  SNAP_DESKTOP_RUNTIME: $SNAP/gnome-platform
  GTK_USE_PORTAL: '1'
`;

test("Snap metadata requires the reviewed strict native contract", () => {
  assert.equal(snapArtifactName("0.2.16", "arm64"), "dsh-desktop-community_0.2.16_arm64.snap");
  assert.deepEqual(snapMetadataProblems(metadata, { version: "0.2.16", architecture: "arm64" }), []);
  assert.ok(
    snapMetadataProblems(metadata.replace("confinement: strict", "confinement: classic"), {
      version: "0.2.16",
      architecture: "arm64",
    }).some((problem) => problem.includes("classic")),
  );
  assert.ok(
    snapMetadataProblems(metadata.replace("- arm64", "- home"), {
      version: "0.2.16",
      architecture: "arm64",
    }).some((problem) => problem.includes("forbidden")),
  );
  assert.ok(
    snapMetadataProblems(metadata.replace("gpu-2404-wrapper", "unexpected-wrapper"), {
      version: "0.2.16",
      architecture: "arm64",
    }).some((problem) => problem.includes("command chain")),
  );
  assert.ok(
    snapMetadataProblems(metadata.replace("- command-chain\n", ""), {
      version: "0.2.16",
      architecture: "arm64",
    }).some((problem) => problem.includes("assumes")),
  );
  assert.ok(
    snapMetadataProblems(
      metadata.replaceAll("aarch64-linux-gnu", "x86_64-linux-gnu"),
      { version: "0.2.16", architecture: "arm64" },
    ).some((problem) => problem.includes("WebKit layout")),
  );
  const amd64Metadata = metadata
    .replace("- arm64", "- amd64")
    .replaceAll("aarch64-linux-gnu", "x86_64-linux-gnu");
  assert.deepEqual(snapMetadataProblems(amd64Metadata, { version: "0.2.16", architecture: "amd64" }), []);
});

test("Snap provenance binds architecture, version, hash, and source commit", () => {
  const digest = "a".repeat(64);
  const provenance = {
    schema: SNAP_PROVENANCE_SCHEMA,
    name: "dsh-desktop-community",
    version: "0.2.16",
    arch: "arm64",
    snapArchitecture: "arm64",
    sourceCommit: "b".repeat(40),
    sourceDeb: { sha256: "c".repeat(64) },
    snap: { sha256: digest },
    snapcraftVersion: "snapcraft 9.0.1",
  };
  assert.deepEqual(
    snapProvenanceProblems(provenance, {
      version: "0.2.16",
      arch: "arm64",
      snapArchitecture: "arm64",
      snapSha256: digest,
      sourceCommit: "b".repeat(40),
    }),
    [],
  );
  assert.ok(
    snapProvenanceProblems(
      { ...provenance, snap: { sha256: "d".repeat(64) } },
      {
        version: "0.2.16",
        arch: "arm64",
        snapArchitecture: "arm64",
        snapSha256: digest,
        sourceCommit: "b".repeat(40),
      },
    ).some((problem) => problem.includes("snap.sha256")),
  );
});
