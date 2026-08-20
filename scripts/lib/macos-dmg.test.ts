import { test } from "node:test";
import assert from "node:assert/strict";
import { dmgArtifactName, macosDmgPaths } from "./macos-dmg.ts";

test("DMG names match the public Tauri-compatible architecture contract", () => {
  assert.equal(dmgArtifactName("DSH Desktop", "1.2.3", "x64"), "DSH Desktop_1.2.3_x64.dmg");
  assert.equal(
    dmgArtifactName("DSH Desktop", "1.2.3", "arm64"),
    "DSH Desktop_1.2.3_aarch64.dmg",
  );
});

test("DMG paths stay in the fixed release bundle directories", () => {
  assert.deepEqual(macosDmgPaths("/repo", "DSH Desktop", "1.2.3", "x64"), {
    appDirectory: "/repo/target/release/bundle/macos",
    outputDirectory: "/repo/target/release/bundle/dmg",
    output: "/repo/target/release/bundle/dmg/DSH Desktop_1.2.3_x64.dmg",
  });
});

test("DMG naming rejects path traversal and empty metadata", () => {
  for (const productName of ["", ".", "..", "../DSH", "DSH/Desktop", "DSH\\Desktop"]) {
    assert.throws(() => dmgArtifactName(productName, "1.2.3", "x64"), /safe filename/);
  }
  assert.throws(() => dmgArtifactName("DSH Desktop", "../1.2.3", "x64"), /safe filename/);
  assert.throws(() => dmgArtifactName(" DSH Desktop", "1.2.3", "x64"), /safe filename/);
  assert.throws(
    () => dmgArtifactName("DSH Desktop", "1.2.3", "powerpc" as never),
    /unsupported DMG architecture/,
  );
});
