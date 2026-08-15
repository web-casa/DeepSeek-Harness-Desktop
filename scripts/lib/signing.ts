// Pure decision/parsing logic for verify-signing.ts — separated so the
// unit tests can import it without executing the CLI body (which calls
// fail()/exit at module top level).

export type Env = Record<string, string | undefined>;

export function expectedSigned(bundleType: string, env: Env): boolean {
  return bundleType === "dmg"
    ? Boolean(env.APPLE_CERTIFICATE)
    : Boolean(env.WINDOWS_CERTIFICATE);
}

/// First non-empty trimmed line of `(Get-AuthenticodeSignature …).Status`:
/// "Valid" / "NotSigned" / "HashMismatch" / "NotTrusted" / "UnknownError".
export function parseAuthenticode(stdout: string): string | null {
  const first = stdout
    .split(/\r?\n/)
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  return first ?? null;
}

/// spawnSync reports a missing/unspawnable tool as status null + error — a
/// shape that must never be mistaken for a verification RESULT (that would
/// make the unsigned branch fail open).
export function toolRan(res: { status: number | null; error?: Error }): boolean {
  return res.status !== null && !res.error;
}
