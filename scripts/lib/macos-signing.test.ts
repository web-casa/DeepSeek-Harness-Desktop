import test from "node:test";
import assert from "node:assert/strict";
import {
  hasHardenedRuntimeFlag,
  isMachOMagic,
  parseCodesignIdentities,
  parseKeychainList,
} from "./macos-signing.ts";

test("recognizes hardened runtime in the actual codesign CodeDirectory field", () => {
  const details = `Executable=/tmp/node
CodeDirectory v=20500 size=123 flags=0x10000(runtime) hashes=1+7 location=embedded
Authority=Developer ID Application: Example (TEAM123456)
Timestamp=Aug 20, 2026 at 1:23:45 PM
TeamIdentifier=TEAM123456
`;
  assert.equal(hasHardenedRuntimeFlag(details), true);
  assert.equal(
    hasHardenedRuntimeFlag("CodeDirectory v=20500 flags=0x0(none)\nflags=0x10000(runtime)"),
    false,
  );
});

test("recognizes every supported Mach-O and universal magic", () => {
  for (const magic of [
    "feedface",
    "cefaedfe",
    "feedfacf",
    "cffaedfe",
    "cafebabe",
    "bebafeca",
    "cafebabf",
    "bfbafeca",
  ]) {
    assert.equal(isMachOMagic(Buffer.from(magic, "hex")), true, magic);
  }
  assert.equal(isMachOMagic(Buffer.from("7f454c46", "hex")), false, "ELF");
  assert.equal(isMachOMagic(Buffer.from("4d5a9000", "hex")), false, "PE");
  assert.equal(isMachOMagic(Buffer.from([0xca, 0xfe, 0xba])), false, "short header");
});

test("parses codesigning identities without accepting summary text", () => {
  const output = `
  1) ABCDEF0123456789 "Developer ID Application: Example (TEAM123456)"
     1 valid identities found
  `;
  assert.deepEqual(parseCodesignIdentities(output), [
    "Developer ID Application: Example (TEAM123456)",
  ]);
  assert.deepEqual(parseCodesignIdentities("0 valid identities found"), []);
});

test("parses quoted security keychain output including spaces", () => {
  assert.deepEqual(
    parseKeychainList('    "/Users/runner/Library/Keychains/login.keychain-db"\n"/tmp/a b.keychain-db"\n'),
    [
      "/Users/runner/Library/Keychains/login.keychain-db",
      "/tmp/a b.keychain-db",
    ],
  );
});
