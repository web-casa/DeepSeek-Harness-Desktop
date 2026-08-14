// Fully materialize a directory tree: dereference every symlink/junction
// into real files and directories, hardlinking regular files so repeated
// symlink targets (pnpm's shared virtual store) cost no extra space.
//
// Result: a self-contained tree with ZERO links — relocatable anywhere,
// works on Windows without Developer Mode, survives NSIS/DMG bundling.
//
// Node's cpSync({dereference:true}) does NOT dereference nested symlinks in
// recursive mode (verified empirically), which is why this exists.

import {
  lstatSync,
  readlinkSync,
  realpathSync,
  mkdirSync,
  readdirSync,
  linkSync,
  copyFileSync,
} from "node:fs";
import { join, dirname } from "node:path";

export function materialize(src: string, dest: string): void {
  materializeInner(src, dest, new Set());
}

function materializeInner(src: string, dest: string, ancestors: Set<string>): void {
  const stat = lstatSync(src);

  if (stat.isSymbolicLink()) {
    const target = readlinkSync(src);
    const resolved = join(dirname(src), target);
    const rp = realpathSync(resolved); // broken links fail loudly — good
    if (ancestors.has(rp)) {
      throw new Error(`symlink cycle at ${src} -> ${resolved}`);
    }
    // The directory branch below registers its own realpath; recursing
    // straight in avoids double-registering the same realpath here.
    materializeInner(resolved, dest, ancestors);
    return;
  }

  if (stat.isDirectory()) {
    // Windows junctions: lstat reports them as symlinks; this branch handles
    // plain directories (junction handling is covered above via isSymbolicLink).
    const rp = realpathSync(src);
    if (ancestors.has(rp)) {
      throw new Error(`directory cycle at ${src}`);
    }
    ancestors.add(rp);
    mkdirSync(dest, { recursive: true });
    for (const name of readdirSync(src)) {
      materializeInner(join(src, name), join(dest, name), ancestors);
    }
    ancestors.delete(rp);
    return;
  }

  if (stat.isFile()) {
    mkdirSync(dirname(dest), { recursive: true });
    try {
      // Hardlink: repeated targets (pnpm dedup) share one inode.
      linkSync(src, dest);
    } catch {
      // EXDEV / no-hardlink filesystems: fall back to a real copy.
      copyFileSync(src, dest);
    }
    return;
  }

  throw new Error(`unsupported entry type at ${src} (${stat.isSymbolicLink() ? "symlink" : "other"})`);
}
