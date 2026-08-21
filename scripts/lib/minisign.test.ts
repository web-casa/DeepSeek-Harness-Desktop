import { test } from "node:test";
import assert from "node:assert/strict";
import {
  createHash,
  generateKeyPairSync,
  sign as signEd25519,
} from "node:crypto";
import { verifyTauriUpdaterSignature } from "./minisign.ts";

function signedFixture(artifact: Buffer): { publicKey: string; signature: string } {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const publicDer = publicKey.export({ format: "der", type: "spki" });
  const rawPublicKey = publicDer.subarray(publicDer.length - 32);
  const keyId = Buffer.from("0102030405060708", "hex");
  const publicBinary = Buffer.concat([Buffer.from("Ed"), keyId, rawPublicKey]);
  const publicFile = `untrusted comment: test key\n${publicBinary.toString("base64")}\n`;

  const digest = createHash("blake2b512").update(artifact).digest();
  const artifactSignature = signEd25519(null, digest, privateKey);
  const trustedComment = "timestamp:1787241600\tfile:fixture.exe\tprehashed";
  const globalSignature = signEd25519(
    null,
    Buffer.concat([artifactSignature, Buffer.from(trustedComment)]),
    privateKey,
  );
  const signatureBinary = Buffer.concat([
    Buffer.from("ED"),
    keyId,
    artifactSignature,
  ]);
  const signatureFile = [
    "untrusted comment: test signature",
    signatureBinary.toString("base64"),
    `trusted comment: ${trustedComment}`,
    globalSignature.toString("base64"),
    "",
  ].join("\n");

  return {
    publicKey: Buffer.from(publicFile).toString("base64"),
    signature: Buffer.from(signatureFile).toString("base64"),
  };
}

test("verifies the prehashed updater format consumed by Tauri", () => {
  const artifact = Buffer.from("reviewed updater artifact");
  const fixture = signedFixture(artifact);
  assert.doesNotThrow(() =>
    verifyTauriUpdaterSignature(artifact, fixture.signature, fixture.publicKey),
  );
});

test("rejects tampered artifacts and malformed outer base64", () => {
  const artifact = Buffer.from("reviewed updater artifact");
  const fixture = signedFixture(artifact);
  assert.throws(
    () =>
      verifyTauriUpdaterSignature(
        Buffer.from("tampered updater artifact"),
        fixture.signature,
        fixture.publicKey,
      ),
    /artifact signature is invalid/,
  );
  assert.throws(
    () => verifyTauriUpdaterSignature(artifact, `${fixture.signature}!`, fixture.publicKey),
    /canonical base64/,
  );
});

test("rejects a signature made by a different updater key", () => {
  const artifact = Buffer.from("reviewed updater artifact");
  const trusted = signedFixture(artifact);
  const attacker = signedFixture(artifact);
  assert.throws(
    () => verifyTauriUpdaterSignature(artifact, attacker.signature, trusted.publicKey),
    /key id does not match|artifact signature is invalid/,
  );
});
