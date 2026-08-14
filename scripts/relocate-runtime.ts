// Copy the staged runtime to a different directory, fully materialized:
// the destination must be a self-contained real tree (no symlinks/junctions),
// exactly like what ships inside the app bundle.

import { mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { repoRoot, runtimeDir, tmpDir, ok } from "./lib/common.ts";
import { materialize } from "./lib/materialize.ts";

const destination = join(repoRoot, ".tmp", "relocated-runtime");
mkdirSync(tmpDir, { recursive: true });
rmSync(destination, { recursive: true, force: true });

materialize(runtimeDir, destination);

ok(`runtime relocated to ${destination}`);
