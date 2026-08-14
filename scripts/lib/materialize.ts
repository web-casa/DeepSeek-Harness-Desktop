// Fully materialize a directory tree: dereference every symlink/junction
// into real files and directories, hardlinking regular files so repeated
// symlink targets (pnpm's shared virtual store) cost no extra space.
//
// Result: a self-contained tree with ZERO links — relocatable anywhere,
// works on Windows without Developer Mode, survives NSIS/DMG bundling.
//
// Node's cpSync({dereference:true}) does NOT dereference nested symlinks in
// recursive mode (verified empirically), which is why this exists.
//
// Security: every link target is canonicalized and must live INSIDE the
// source root — a crafted dependency cannot smuggle arbitrary host files
// into the distributable installer.

import {
  lstatSync,
  readlinkSync,
  realpathSync,
  mkdirSync,
  readdirSync,
  linkSync,
  copyFileSync,
} from "node:fs";
import { join, dirname, resolve, relative, isAbsolute, sep } from "node:path";

export function materialize(src: string, dest: string): void {
  const root = realpathSync(src);
  materializeInner(src, dest, new Set(), root);
}

function assertInsideRoot(canonical: string, root: string, origin: string): void {
  const rel = relative(root, canonical);
  if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(
      `symlink at ${origin} resolves outside the source tree (${canonical}); refusing to bundle external content`,
    );
  }
}

function materializeInner(src: string, dest: string, ancestors: Set<string>, root: string): void {
  const stat = lstatSync(src);

  if (stat.isSymbolicLink()) {
    const target = readlinkSync(src);
    // path.resolve handles both relative (../../x) and absolute targets;
    // join() would silently treat an absolute target as relative.
    const resolvedAbs = resolve(dirname(src), target);
    const rp = realpathSync(resolvedAbs); // broken links fail loudly — good
    assertInsideRoot(rp, root, src);
    if (ancestors.has(rp)) {
      throw new Error(`symlink cycle at ${src} -> ${resolvedAbs}`);
    }
    materializeInner(rp, dest, ancestors, root);
    return;
  }

  if (stat.isDirectory()) {
    // Windows junctions may surface here instead of the symlink branch; the
    // realpath check keeps them contained regardless of lstat reporting.
    const rp = realpathSync(src);
    assertInsideRoot(rp, root, src);
    if (ancestors.has(rp)) {
      throw new Error(`directory cycle at ${src}`);
    }
    ancestors.add(rp);
    mkdirSync(dest, { recursive: true });
    for (const name of readdirSync(src)) {
      materializeInner(join(src, name), join(dest, name), ancestors, root);
    }
    ancestors.delete(rp);
    return;
  }

  if (stat.isFile()) {
    mkdirSync(dirname(dest), { recursive: true });
    try {
      // Hardlink: repeated targets (pnpm dedup) share one inode.
      linkSync(src, dest);
    } catch (e) {
      // Only fall back to a real copy when hardlinks are unavailable; real
      // failures (ENOSPC, EIO, …) must surface instead of being retried.
      const code = (e as NodeJS.ErrnoException).code;
      if (code === "EXDEV" || code === "EPERM" || code === "EACCES" || code === "ENOSYS") {
        copyFileSync(src, dest);
      } else {
        throw e;
      }
    }
    return;
  }

  throw new Error(`unsupported entry type at ${src} (${stat.isSymbolicLink() ? "symlink" : "other"})`);
}
