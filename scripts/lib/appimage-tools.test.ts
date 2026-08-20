import { test } from "node:test";
import assert from "node:assert/strict";
import {
  appImageToolDefinitionProblems,
  appImageToolsForArch,
} from "./appimage-tools.ts";

for (const arch of ["x64", "arm64"] as const) {
  test(`${arch} AppImage tool sources are immutable and SHA-256 pinned`, () => {
    assert.deepEqual(appImageToolDefinitionProblems(arch), []);
    assert.equal(appImageToolsForArch(arch).length, 5);
  });
}

test("architecture-specific tools do not accidentally share binary hashes", () => {
  const x64 = appImageToolsForArch("x64");
  const arm64 = appImageToolsForArch("arm64");
  const x64BinaryHashes = x64.slice(0, 3).map((tool) => tool.sha256);
  const arm64BinaryHashes = new Set(
    arm64.slice(0, 3).map((tool) => tool.sha256),
  );
  assert.equal(x64BinaryHashes.some((hash) => arm64BinaryHashes.has(hash)), false);
  assert.deepEqual(x64.slice(3), arm64.slice(3));
});
