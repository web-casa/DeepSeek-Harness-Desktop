// The staged runtime must be a fully materialized tree: zero symlinks of any
// kind. prepare-harness dereferences everything (lib/materialize.ts), so any
// link here means the staging step regressed and the bundle would not be
// self-contained on Windows user machines.
//
// Windows junctions report as directories (isSymbolicLink() === false), so a
// readlink probe covers them too: a junction answers readlink with its target,
// a real directory throws.

import { existsSync, lstatSync, readdirSync, readlinkSync } from "node:fs";
import { join } from "node:path";
import { harnessDir, fail, ok } from "./lib/common.ts";

const nodeModules = join(harnessDir, "node_modules");
if (!existsSync(nodeModules)) fail(`staged node_modules missing at ${nodeModules}`);

const links: string[] = [];

function scan(dir: string): void {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    const stat = lstatSync(path);
    if (stat.isSymbolicLink()) {
      links.push(`${path} -> ${readlinkSync(path)}`);
      continue;
    }
    if (stat.isDirectory()) {
      // Junction probe: a real directory throws, a junction yields its target.
      try {
        const target = readlinkSync(path);
        links.push(`${path} -> ${target} (junction)`);
        continue;
      } catch {
        /* plain directory */
      }
      scan(path);
    }
  }
}

scan(nodeModules);
if (links.length > 0) {
  fail(
    `staged runtime contains ${links.length} link(s); it must be fully materialized:\n` +
      links.slice(0, 10).map((link) => `  ${link}`).join("\n"),
  );
}

ok("staged runtime is fully materialized (no symlinks/junctions)");
