import assert from "node:assert/strict";
import test from "node:test";
import { normalizeMsixEntryName } from "./msix.ts";

test("normalizes AppX URI escaping and Windows separators", () => {
  assert.equal(
    normalizeMsixEntryName(
      "runtime\\harness\\node_modules\\%40deepseek-ai\\dsh\\package.json",
    ),
    "runtime/harness/node_modules/@deepseek-ai/dsh/package.json",
  );
});

test("rejects encoded separators, traversal, and malformed escapes", () => {
  for (const entry of ["safe/%2fhidden", "safe/%5chidden", "safe/%2e%2e/file", "bad/%xy"]) {
    assert.throws(() => normalizeMsixEntryName(entry));
  }
});
