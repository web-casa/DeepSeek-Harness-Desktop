// Copy the staged runtime to a different directory without rewriting symlinks.

import { cpSync, mkdirSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { repoRoot, runtimeDir, tmpDir, fail, ok } from "./lib/common.ts";

const destination = join(repoRoot, ".tmp", "relocated-runtime");
mkdirSync(tmpDir, { recursive: true });
rmSync(destination, { recursive: true, force: true });

if (process.platform !== "win32") {
  cpSync(runtimeDir, destination, { recursive: true, verbatimSymlinks: true });
} else {
  const res = spawnSync("robocopy", [
    runtimeDir,
    destination,
    "/E",
    "/COPY:DAT",
    "/DCOPY:DAT",
    "/R:1",
    "/W:1",
    "/NFL",
    "/NDL",
    "/NJH",
    "/NJS",
  ], { stdio: "inherit" });
  if (res.status === null || res.status < 0 || res.status > 7) {
    fail(`robocopy failed with exit code ${res.status}`);
  }
}

ok(`runtime relocated to ${destination}`);
