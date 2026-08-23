import assert from "node:assert/strict";
import test from "node:test";
import {
  githubReleaseAssetName,
  parseSha256Sidecar,
  releaseChecksumProblems,
  sha256SidecarContent,
} from "./release-checksums.ts";

const digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const installer = {
  id: 10,
  name: "DSH.Desktop_0.2.15_x64-setup.exe",
  digest: `sha256:${digest}`,
};
const sidecar = { id: 11, name: `${installer.name}.sha256` };

test("checksum sidecars use the GitHub Release asset name, not the local name", () => {
  assert.equal(
    githubReleaseAssetName("DSH Desktop_0.2.15_x64-setup.exe"),
    installer.name,
  );
  assert.equal(
    sha256SidecarContent(digest, "DSH Desktop_0.2.15_x64-setup.exe"),
    `${digest}  ${installer.name}\n`,
  );
  assert.throws(() => githubReleaseAssetName("../DSH Desktop.exe"), /unsafe local release asset filename/);
  assert.throws(() => githubReleaseAssetName("DSH\tDesktop.exe"), /unsafe local release asset filename/);
});

test("draft release checksum audit accepts the exact uploaded asset identity and digest", () => {
  const contents = new Map([[sidecar.id, `${digest}  ${installer.name}\n`]]);
  assert.deepEqual(releaseChecksumProblems([installer], [sidecar], contents), []);
  assert.deepEqual(parseSha256Sidecar(contents.get(sidecar.id) ?? ""), {
    digest,
    filename: installer.name,
  });
});

test("draft release checksum audit rejects upload-time filename and digest drift", () => {
  const oldLocalName = "DSH Desktop_0.2.15_x64-setup.exe";
  const wrongName = new Map([[sidecar.id, `${digest}  ${oldLocalName}\n`]]);
  assert.deepEqual(releaseChecksumProblems([installer], [sidecar], wrongName), [
    `SHA-256 sidecar filename mismatch: ${sidecar.name} names ${oldLocalName}, published asset is ${installer.name}`,
  ]);

  const wrongDigest = new Map([[sidecar.id, `${"f".repeat(64)}  ${installer.name}\n`]]);
  assert.deepEqual(releaseChecksumProblems([installer], [sidecar], wrongDigest), [
    `SHA-256 sidecar digest mismatch: ${sidecar.name} has sha256:${"f".repeat(64)}, GitHub reports sha256:${digest} for ${installer.name}`,
  ]);
});

test("draft release checksum audit rejects missing and orphan sidecars", () => {
  assert.deepEqual(releaseChecksumProblems([installer], [], new Map()), [
    `missing SHA-256 sidecar for published asset: ${installer.name}`,
  ]);
  assert.deepEqual(
    releaseChecksumProblems([], [{ id: 12, name: "unrelated.txt.sha256" }], new Map()),
    ["orphan SHA-256 sidecar on public release: unrelated.txt.sha256"],
  );
});
