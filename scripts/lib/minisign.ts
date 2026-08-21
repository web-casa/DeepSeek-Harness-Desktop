import {
  createHash,
  createPublicKey,
  timingSafeEqual,
  verify as verifyEd25519,
} from "node:crypto";

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const TRUSTED_COMMENT_PREFIX = "trusted comment: ";

function decodeBase64Strict(value: string, label: string): Buffer {
  const encoded = value.trim();
  if (
    encoded.length === 0 ||
    encoded.length % 4 !== 0 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)
  ) {
    throw new Error(`${label} is not canonical base64`);
  }
  const decoded = Buffer.from(encoded, "base64");
  if (decoded.toString("base64") !== encoded) {
    throw new Error(`${label} is not canonical base64`);
  }
  return decoded;
}

function decodeUtf8Strict(value: Buffer, label: string): string {
  const decoded = new TextDecoder("utf-8", { fatal: true }).decode(value);
  if (Buffer.from(decoded, "utf8").compare(value) !== 0) {
    throw new Error(`${label} is not valid UTF-8`);
  }
  return decoded;
}

function algorithm(binary: Buffer, label: string): "Ed" | "ED" {
  const value = binary.subarray(0, 2).toString("ascii");
  if (value !== "Ed" && value !== "ED") {
    throw new Error(`${label} uses an unsupported algorithm`);
  }
  return value;
}

interface ParsedPublicKey {
  keyId: Buffer;
  rawKey: Buffer;
}

function parsePublicKey(encodedPublicKey: string): ParsedPublicKey {
  const text = decodeUtf8Strict(
    decodeBase64Strict(encodedPublicKey, "updater public key"),
    "updater public key",
  );
  const lines = text.trimEnd().split(/\r?\n/);
  if (lines.length !== 2) throw new Error("updater public key must contain exactly two lines");
  const binary = decodeBase64Strict(lines[1] ?? "", "minisign public key");
  if (binary.length !== 42) throw new Error("minisign public key has an invalid length");
  algorithm(binary, "minisign public key");
  return { keyId: binary.subarray(2, 10), rawKey: binary.subarray(10, 42) };
}

interface ParsedSignature {
  keyId: Buffer;
  signature: Buffer;
  globalSignature: Buffer;
  trustedComment: string;
  prehashed: boolean;
}

function parseSignature(encodedSignature: string): ParsedSignature {
  const text = decodeUtf8Strict(
    decodeBase64Strict(encodedSignature, "updater signature"),
    "updater signature",
  );
  const lines = text.trimEnd().split(/\r?\n/);
  if (lines.length !== 4) throw new Error("updater signature must contain exactly four lines");
  const binary = decodeBase64Strict(lines[1] ?? "", "minisign signature");
  const globalSignature = decodeBase64Strict(
    lines[3] ?? "",
    "minisign global signature",
  );
  if (binary.length !== 74) throw new Error("minisign signature has an invalid length");
  if (globalSignature.length !== 64) {
    throw new Error("minisign global signature has an invalid length");
  }
  const signatureAlgorithm = algorithm(binary, "minisign signature");
  const trustedComment = lines[2] ?? "";
  if (!trustedComment.startsWith(TRUSTED_COMMENT_PREFIX)) {
    throw new Error("minisign signature is missing its trusted comment");
  }
  return {
    keyId: binary.subarray(2, 10),
    signature: binary.subarray(10, 74),
    globalSignature,
    trustedComment: trustedComment.slice(TRUSTED_COMMENT_PREFIX.length),
    prehashed: signatureAlgorithm === "ED",
  };
}

/**
 * Verify the base64-wrapped minisign format consumed by tauri-plugin-updater.
 * Both the artifact signature and minisign's trusted-comment signature must
 * validate under the exact updater key embedded in tauri.conf.json.
 */
export function verifyTauriUpdaterSignature(
  artifact: Uint8Array,
  encodedSignature: string,
  encodedPublicKey: string,
): void {
  const publicKey = parsePublicKey(encodedPublicKey);
  const signature = parseSignature(encodedSignature);
  if (!timingSafeEqual(publicKey.keyId, signature.keyId)) {
    throw new Error("updater signature key id does not match the embedded public key");
  }

  const key = createPublicKey({
    key: Buffer.concat([ED25519_SPKI_PREFIX, publicKey.rawKey]),
    format: "der",
    type: "spki",
  });
  const message = signature.prehashed
    ? createHash("blake2b512").update(artifact).digest()
    : Buffer.from(artifact);
  if (!verifyEd25519(null, message, key, signature.signature)) {
    throw new Error("updater artifact signature is invalid");
  }

  const globalMessage = Buffer.concat([
    signature.signature,
    Buffer.from(signature.trustedComment, "utf8"),
  ]);
  if (!verifyEd25519(null, globalMessage, key, signature.globalSignature)) {
    throw new Error("updater trusted-comment signature is invalid");
  }
}
