import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const sourceRoot = new URL("../../snap/command-chain/", import.meta.url);
const canRunBash = existsSync("/bin/bash");

function writeExecutable(path: string, contents: string): void {
  writeFileSync(path, contents, { mode: 0o755 });
}

test("local Snap command chain preserves GPU and GNOME provider environment", { skip: !canRunBash }, () => {
  const root = mkdtempSync(join(tmpdir(), "dsh-snap-chain-"));
  try {
    const snap = join(root, "snap-root");
    const chain = join(snap, "snap", "command-chain");
    mkdirSync(chain, { recursive: true, mode: 0o700 });
    for (const script of ["gpu-2404-wrapper", "desktop-launch", "run"] as const) {
      const target = join(chain, script);
      cpSync(new URL(script, sourceRoot), target);
      chmodSync(target, 0o755);
    }

    const providerDir = join(snap, "gpu-2404", "bin");
    mkdirSync(providerDir, { recursive: true, mode: 0o700 });
    writeExecutable(
      join(providerDir, "gpu-2404-provider-wrapper"),
      `#!/bin/bash
set -euo pipefail
export DSH_SNAP_TEST_GPU=connected
exec "$@"
`,
    );

    const gnomeDir = join(snap, "gnome-platform", "command-chain");
    mkdirSync(gnomeDir, { recursive: true, mode: 0o700 });
    writeExecutable(
      join(gnomeDir, "desktop-launch"),
      `#!/bin/bash
export DSH_SNAP_TEST_GNOME=connected
# The real gnome-46 launcher reads optional first-run state before defining
# it. This fails if a local relay accidentally leaks nounset into it.
if [ "$DSH_SNAP_TEST_OPTIONAL" = "configured" ]; then
  export DSH_SNAP_TEST_OPTIONAL=seen
fi
exec "$@"
`,
    );

    const finalCommand = join(root, "final-command");
    writeExecutable(
      finalCommand,
      `#!/bin/sh
printf '%s|%s|%s\\n' "$DSH_SNAP_TEST_GPU" "$DSH_SNAP_TEST_GNOME" "$1"
`,
    );
    const result = spawnSync(
      join(chain, "gpu-2404-wrapper"),
      [join(chain, "desktop-launch"), finalCommand, "dsharness://plugin/install?name=checked"],
      {
        encoding: "utf8",
        env: { ...process.env, SNAP: snap },
      },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, "connected|connected|dsharness://plugin/install?name=checked\n");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
