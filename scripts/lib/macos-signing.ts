// Pure helpers shared by the macOS nested-runtime signer and Linux-hosted
// unit tests.  Do not identify native code from file names or executable
// bits: npm packages contain extensionless helpers, `.node` addons and
// `.dylib` libraries with inconsistent modes.

const MACH_O_MAGICS = new Set([
  0xfeedface, // 32-bit Mach-O
  0xcefaedfe, // 32-bit Mach-O, reverse byte order
  0xfeedfacf, // 64-bit Mach-O
  0xcffaedfe, // 64-bit Mach-O, reverse byte order
  0xcafebabe, // universal binary
  0xbebafeca, // universal binary, reverse byte order
  0xcafebabf, // universal binary with 64-bit fat_arch entries
  0xbfbafeca, // universal binary with 64-bit fat_arch entries, reversed
]);

export function isMachOMagic(header: Uint8Array): boolean {
  if (header.byteLength < 4) return false;
  const magic =
    ((header[0] << 24) | (header[1] << 16) | (header[2] << 8) | header[3]) >>> 0;
  return MACH_O_MAGICS.has(magic);
}

export function parseCodesignIdentities(stdout: string): string[] {
  const identities: string[] = [];
  for (const line of stdout.split(/\r?\n/)) {
    const match = line.match(/^\s*\d+\)\s+[0-9A-F]+\s+"([^"]+)"\s*$/i);
    if (match) identities.push(match[1]);
  }
  return identities;
}

export function parseKeychainList(stdout: string): string[] {
  const keychains: string[] = [];
  for (const line of stdout.split(/\r?\n/)) {
    const path = line.trim().replace(/^"|"$/g, "");
    if (path) keychains.push(path);
  }
  return keychains;
}

export function hasHardenedRuntimeFlag(codesignDetails: string): boolean {
  return /^CodeDirectory\b[^\r\n]*\bflags=[^\r\n]*\bruntime\b/m.test(codesignDetails);
}
