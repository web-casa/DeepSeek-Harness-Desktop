import { test } from "node:test";
import assert from "node:assert/strict";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pruneGlibcKoffiMuslVariant } from "./runtime-prune.ts";

function fixture(arch: "x64" | "arm64"): {
  root: string;
  packageRoot: string;
} {
  const root = mkdtempSync(join(tmpdir(), "dsh-runtime-prune-"));
  const packageRoot = join(root, "@koromix", `koffi-linux-${arch}`);
  for (const variant of [`linux_${arch}`, `musl_${arch}`]) {
    mkdirSync(join(packageRoot, variant), { recursive: true });
    writeFileSync(join(packageRoot, variant, "koffi.node"), variant);
  }
  writeFileSync(
    join(packageRoot, "package.json"),
    JSON.stringify({ name: `@koromix/koffi-linux-${arch}` }),
  );
  return { root, packageRoot };
}

for (const arch of ["x64", "arm64"] as const) {
  test(`prunes only the ${arch} musl Koffi variant`, () => {
    const { root, packageRoot } = fixture(arch);
    try {
      const removed = pruneGlibcKoffiMuslVariant(root, arch);
      assert.equal(removed, join(packageRoot, `musl_${arch}`));
      assert.equal(existsSync(join(packageRoot, `musl_${arch}`)), false);
      assert.equal(existsSync(join(packageRoot, `linux_${arch}`, "koffi.node")), true);
      assert.equal(existsSync(join(packageRoot, "package.json")), true);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("fails closed when the musl directory contains an unreviewed file", () => {
  const { root, packageRoot } = fixture("arm64");
  try {
    writeFileSync(join(packageRoot, "musl_arm64", "README"), "changed layout");
    assert.throws(
      () => pruneGlibcKoffiMuslVariant(root, "arm64"),
      /refusing to prune changed Koffi musl layout/,
    );
    assert.equal(existsSync(join(packageRoot, "musl_arm64", "koffi.node")), true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("rejects an architecture outside the reviewed release matrix", () => {
  assert.throws(
    () => pruneGlibcKoffiMuslVariant("unused", "riscv64"),
    /unsupported glibc Linux architecture/,
  );
});
