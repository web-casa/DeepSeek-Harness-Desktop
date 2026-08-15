// Pure package-layout logic for prepare-harness.ts's license collector.

/// A directory counts as a package root only when its path has the SHAPE of
/// one: <top>/pkg, <top>/@scope/pkg, */node_modules/pkg, or
/// */node_modules/@scope/pkg. Inner package.json files (the
/// {"type":"module"} stubs in dist/esm, cjs, helpers/…) mark file-layout
/// hints, not separate packages.
export function isPackageRoot(segs: string[]): boolean {
  const n = segs.length;
  if (n === 0) return false;
  if (n === 1) return true; // <top>/pkg
  if (n === 2 && segs[0].startsWith("@")) return true; // <top>/@scope/pkg
  if (segs[n - 2] === "node_modules") return true; // …/node_modules/pkg
  if (n >= 3 && segs[n - 3] === "node_modules" && segs[n - 2].startsWith("@")) {
    return true; // …/node_modules/@scope/pkg
  }
  return false;
}
