// Verify staged pnpm links remain relative so the bundled runtime is relocatable.

import { existsSync, lstatSync, readdirSync, readlinkSync } from "node:fs";
import { join } from "node:path";
import { harnessDir, fail, ok } from "./lib/common.ts";

const nodeModules = join(harnessDir, "node_modules");
if (!existsSync(nodeModules)) fail(`staged node_modules missing at ${nodeModules}`);

const absoluteLinks: string[] = [];

function scan(dir: string): void {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    const stat = lstatSync(path);
    if (stat.isSymbolicLink()) {
      const target = readlinkSync(path);
      if (target.startsWith("/") || /^[A-Za-z]:[\\/]/.test(target)) {
        absoluteLinks.push(`${path} -> ${target}`);
      }
      continue;
    }
    if (stat.isDirectory()) scan(path);
  }
}

scan(nodeModules);
if (absoluteLinks.length > 0) {
  fail(
    `found ${absoluteLinks.length} absolute symlink target(s) in staged runtime:\n` +
      absoluteLinks.slice(0, 10).map((link) => `  ${link}`).join("\n"),
  );
}

ok("staged runtime links are relocatable");
