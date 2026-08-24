// Pure contracts shared by the Snap builder and the post-pack verifier.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import type { ReleaseArch } from "./release-artifacts.ts";
import {
  SNAP_APP_PLUGS,
  SNAP_ASSUMES,
  SNAP_BASE,
  SNAP_COMMAND_CHAIN,
  SNAP_DECLARED_PLUGS,
  SNAP_NAME,
  SNAP_PORTAL_REQUIRED_APP_PLUGS,
  SNAP_TITLE,
  type SnapArchitecture,
} from "./snap.ts";

export const SNAP_PROVENANCE_SCHEMA = 1;

export interface SnapProvenance {
  schema: number;
  name: string;
  version: string;
  arch: ReleaseArch;
  snapArchitecture: SnapArchitecture;
  sourceCommit: string;
  sourceDeb: {
    sha256: string;
  };
  snap: {
    sha256: string;
  };
  snapcraftVersion: string;
}

export function sha256File(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

export function snapArtifactName(version: string, architecture: SnapArchitecture): string {
  return `${SNAP_NAME}_${version}_${architecture}.snap`;
}

function scalar(text: string, key: string): string | undefined {
  const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`^${escaped}:\\s*["']?([^"'\\r\\n]+?)["']?\\s*$`, "m")
    .exec(text)?.[1]
    ?.trim();
}

function topLevelSection(text: string, key: string): string | undefined {
  const match = new RegExp(
    // A top-level YAML list can itself start at column zero.  Keep the next-key
    // matcher on one physical line: `[^:]` also matches newlines in JavaScript,
    // which otherwise makes a list item swallow through the following key.
    `^${key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}:\\n([\\s\\S]*?)(?=^[^\\s\\r\\n][^:\\r\\n]*:|(?![\\s\\S]))`,
    "m",
  ).exec(text);
  return match?.[1];
}

function mappingKeys(section: string | undefined): string[] {
  if (!section) return [];
  return [...section.matchAll(/^  ([^\s:#][^:]*):\s*$/gm)].map((match) => match[1]);
}

function topLevelList(section: string | undefined): string[] {
  if (!section) return [];
  return [...section.matchAll(/^\s*-\s+([^\r\n]+)$/gm)].map((match) => match[1].trim());
}

function appSection(metadata: string): string | undefined {
  const apps = topLevelSection(metadata, "apps");
  if (!apps) return undefined;
  const app = new RegExp(
    `^  ${SNAP_NAME.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}:\\n([\\s\\S]*?)(?=^  [^\\s:#][^:]*:|(?![\\s\\S]))`,
    "m",
  ).exec(apps);
  return app?.[1];
}

function listField(section: string | undefined, field: string): string[] {
  if (!section) return [];
  const escaped = field.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`^    ${escaped}:\\n((?:^    - [^\\r\\n]+\\r?\\n?)+)`, "m").exec(section);
  return match ? [...match[1].matchAll(/^    -\s+([^\r\n]+)$/gm)].map((item) => item[1].trim()) : [];
}

function hasLine(metadata: string, line: string): boolean {
  return new RegExp(`^${line.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`, "m").test(metadata);
}

function snapArchTriplet(architecture: SnapArchitecture): string {
  switch (architecture) {
    case "amd64":
      return "x86_64-linux-gnu";
    case "arm64":
      return "aarch64-linux-gnu";
  }
}

export function snapMetadataProblems(
  metadata: string,
  expected: { version: string; architecture: SnapArchitecture },
): string[] {
  const problems: string[] = [];
  for (const [key, wanted] of [
    ["name", SNAP_NAME],
    ["title", SNAP_TITLE],
    ["base", SNAP_BASE],
    ["version", expected.version],
    ["grade", "stable"],
    ["confinement", "strict"],
  ] as const) {
    if (scalar(metadata, key) !== wanted) {
      problems.push(`Snap metadata ${key}=${scalar(metadata, key) ?? "missing"} != ${wanted}`);
    }
  }
  if (/^confinement:\s*(?:classic|devmode)\s*$/m.test(metadata)) {
    problems.push("Snap metadata permits classic or devmode confinement");
  }
  if (/^\s*-\s*(?:home|removable-media)\s*$/m.test(metadata)) {
    problems.push("Snap metadata requests forbidden home or removable-media access");
  }
  const architecture = expected.architecture;
  const supported =
    new RegExp(`^architectures:\\s*\\[${architecture}\\]\\s*$`, "m").test(metadata) ||
    new RegExp(`^architectures:\\n(?:^\\s*-\\s*${architecture}\\s*$\\n?)+`, "m").test(metadata);
  if (!supported) {
    problems.push(`Snap metadata does not declare ${architecture} architecture`);
  }
  if (/^\s*extensions:/m.test(metadata)) {
    problems.push("Snap metadata unexpectedly retains an extension declaration");
  }
  // Snapcraft writes computed assumptions in lexical order.  In particular it
  // derives `command-chain` from the app declaration; the source recipe must
  // not duplicate it.  Require the exact final list so an unexpected snapd
  // feature or a duplicate cannot silently enter the package contract.
  const assumptions = topLevelList(topLevelSection(metadata, "assumes"));
  const expectedAssumptions = [...SNAP_ASSUMES].sort();
  if (assumptions.join(",") !== expectedAssumptions.join(",")) {
    problems.push(`Snap metadata assumes must be ${expectedAssumptions.join(",")}`);
  }

  const app = appSection(metadata);
  if (!app) {
    problems.push(`Snap metadata does not declare ${SNAP_NAME} app`);
  } else {
    if (!hasLine(app, "    command: bin/launch-dsh-desktop")) {
      problems.push("Snap metadata app command diverges from the reviewed launcher");
    }
    const commandChain = listField(app, "command-chain");
    if (commandChain.join(",") !== SNAP_COMMAND_CHAIN.join(",")) {
      problems.push(`Snap metadata command chain must be ${SNAP_COMMAND_CHAIN.join(",")}`);
    }
    const appPlugs = listField(app, "plugs");
    if (appPlugs.join(",") !== SNAP_APP_PLUGS.join(",")) {
      problems.push(`Snap metadata app plugs must be ${SNAP_APP_PLUGS.join(",")}`);
    }
    for (const plug of SNAP_PORTAL_REQUIRED_APP_PLUGS) {
      if (!appPlugs.includes(plug)) {
        problems.push(`Snap metadata must enable ${plug} for the GTK/XDG portal runtime`);
      }
    }
  }

  const declaredPlugs = mappingKeys(topLevelSection(metadata, "plugs"));
  if (declaredPlugs.join(",") !== SNAP_DECLARED_PLUGS.join(",")) {
    problems.push(`Snap metadata declared plugs must be ${SNAP_DECLARED_PLUGS.join(",")}`);
  }
  for (const line of [
    "    target: $SNAP/gpu-2404",
    "    default-provider: mesa-2404",
    "    target: $SNAP/gnome-platform",
    "    default-provider: gnome-46-2404",
    "  SNAP_DESKTOP_RUNTIME: $SNAP/gnome-platform",
    "  GTK_USE_PORTAL: '1'",
  ]) {
    if (!hasLine(metadata, line)) {
      problems.push(`Snap metadata is missing reviewed runtime setting: ${line.trim()}`);
    }
  }
  const archTriplet = snapArchTriplet(expected.architecture);
  // Craft expands CRAFT_ARCH_TRIPLET_BUILD_FOR before serializing the final
  // package metadata.  Check the concrete target triplet, not the source
  // recipe placeholder, so an amd64 payload cannot masquerade as arm64.
  for (const line of [
    `  /usr/lib/${archTriplet}/webkit2gtk-4.0:`,
    `  /usr/lib/${archTriplet}/webkit2gtk-4.1:`,
  ]) {
    if (!hasLine(metadata, line)) {
      problems.push(`Snap metadata is missing reviewed WebKit layout: ${line.trim()}`);
    }
  }
  return problems;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function stringAt(record: Record<string, unknown> | undefined, key: string): string | undefined {
  const value = record?.[key];
  return typeof value === "string" ? value : undefined;
}

export function snapProvenanceProblems(
  raw: unknown,
  expected: {
    version: string;
    arch: ReleaseArch;
    snapArchitecture: SnapArchitecture;
    snapSha256: string;
    sourceCommit?: string;
  },
): string[] {
  const record = asRecord(raw);
  const sourceDeb = asRecord(record?.sourceDeb);
  const snap = asRecord(record?.snap);
  const problems: string[] = [];
  if (record?.schema !== SNAP_PROVENANCE_SCHEMA) {
    problems.push(`Snap provenance schema must be ${SNAP_PROVENANCE_SCHEMA}`);
  }
  for (const [key, wanted] of [
    ["name", SNAP_NAME],
    ["version", expected.version],
    ["arch", expected.arch],
    ["snapArchitecture", expected.snapArchitecture],
  ] as const) {
    if (stringAt(record, key) !== wanted) {
      problems.push(`Snap provenance ${key}=${stringAt(record, key) ?? "missing"} != ${wanted}`);
    }
  }
  const commit = stringAt(record, "sourceCommit");
  if (!/^[0-9a-f]{40}$/i.test(commit ?? "")) {
    problems.push("Snap provenance sourceCommit must be a full Git SHA");
  }
  if (expected.sourceCommit !== undefined && commit !== expected.sourceCommit) {
    problems.push("Snap provenance sourceCommit does not match the checked-out source revision");
  }
  const sourceDebSha = stringAt(sourceDeb, "sha256");
  if (!/^[0-9a-f]{64}$/i.test(sourceDebSha ?? "")) {
    problems.push("Snap provenance sourceDeb.sha256 must be 64 hex");
  }
  if (stringAt(snap, "sha256") !== expected.snapSha256) {
    problems.push("Snap provenance snap.sha256 does not match the verified artifact");
  }
  if (!/^snapcraft\s+\d+\.\d+\.\d+/.test(stringAt(record, "snapcraftVersion") ?? "")) {
    problems.push("Snap provenance snapcraftVersion is malformed");
  }
  return problems;
}
