import test from "node:test";
import assert from "node:assert/strict";
import {
  ascCliDefinitionProblems,
  ascCliDistribution,
  ascCliDownloadUrl,
  ascCliRelease,
} from "./asc-cli.ts";

test("ASC CLI macOS downloads are immutable and SHA-256 pinned", () => {
  assert.deepEqual(ascCliDefinitionProblems(), []);
  assert.equal(ascCliRelease.version, "4.6.0");
  for (const arch of ["x64", "arm64"] as const) {
    const distribution = ascCliDistribution(arch);
    assert.match(distribution.sha256, /^[a-f0-9]{64}$/);
    assert.equal(ascCliDownloadUrl(arch).pathname.endsWith(`/${distribution.file}`), true);
  }
});
