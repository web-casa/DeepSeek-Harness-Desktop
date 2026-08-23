import assert from "node:assert/strict";
import test from "node:test";
import { targetById } from "./release-artifacts.ts";
import { nativeHostProblems } from "./native-host.ts";

const windowsArm64 = targetById("windows-arm64");
const macosX64 = targetById("macos-x64");
if (!windowsArm64 || !macosX64) throw new Error("reviewed native target missing");

test("native host gate accepts the exact reviewed runner triple", () => {
  assert.deepEqual(
    nativeHostProblems(windowsArm64, {
      platform: "win32",
      arch: "arm64",
      rustHost: "aarch64-pc-windows-msvc",
    }),
    [],
  );
});

test("native host gate rejects emulation and cross-compilation", () => {
  assert.deepEqual(
    nativeHostProblems(windowsArm64, {
      platform: "win32",
      arch: "x64",
      rustHost: "x86_64-pc-windows-msvc",
    }),
    [
      "windows-arm64: expected Node architecture arm64, got x64",
      "windows-arm64: expected Rust host aarch64-pc-windows-msvc, got x86_64-pc-windows-msvc",
    ],
  );
  assert.deepEqual(
    nativeHostProblems(macosX64, {
      platform: "darwin",
      arch: "x64",
      rustHost: "aarch64-apple-darwin",
    }),
    [
      "macos-x64: expected Rust host x86_64-apple-darwin, got aarch64-apple-darwin",
    ],
  );
});
