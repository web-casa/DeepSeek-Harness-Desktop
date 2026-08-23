import assert from "node:assert/strict";
import test from "node:test";
import { normalizeMsixEntryName, windowsPeArchitecture } from "./msix.ts";

function peHeader(machine: number): Buffer {
  const header = Buffer.alloc(0x90);
  header[0] = 0x4d;
  header[1] = 0x5a;
  header.writeUInt32LE(0x80, 0x3c);
  header.write("PE\0\0", 0x80, "ascii");
  header.writeUInt16LE(machine, 0x84);
  return header;
}

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

test("reads only reviewed x64 and arm64 PE machine headers", () => {
  assert.equal(windowsPeArchitecture(peHeader(0x8664)), "x64");
  assert.equal(windowsPeArchitecture(peHeader(0xaa64)), "arm64");
  assert.throws(() => windowsPeArchitecture(peHeader(0x014c)), /unreviewed PE machine/);
  assert.throws(() => windowsPeArchitecture(Buffer.alloc(0x40)), /DOS\/PE header/);

  const malformedOffset = peHeader(0x8664);
  malformedOffset.writeUInt32LE(0x200, 0x3c);
  assert.throws(() => windowsPeArchitecture(malformedOffset), /outside the captured/);
});
