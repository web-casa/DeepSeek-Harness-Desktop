/**
 * AppX stores package entry names as URI paths. In particular, MakeAppx
 * percent-encodes npm scope markers (`@scope` -> `%40scope`) in the ZIP
 * central directory even though its build log prints the source path.
 * Decode one segment at a time so an encoded slash cannot manufacture a
 * different path during verification.
 */
export function normalizeMsixEntryName(name: string): string {
  return name
    .replace(/\\/g, "/")
    .split("/")
    .map((segment) => {
      let decoded: string;
      try {
        decoded = decodeURIComponent(segment);
      } catch {
        throw new Error(`malformed percent-encoding in MSIX entry: ${name}`);
      }
      if (decoded === "." || decoded === ".." || /[\\/\0]/.test(decoded)) {
        throw new Error(`unsafe encoded path segment in MSIX entry: ${name}`);
      }
      return decoded;
    })
    .join("/");
}

export type WindowsPeArchitecture = "x64" | "arm64";

const PE_MACHINE_ARCHITECTURES: Readonly<Record<number, WindowsPeArchitecture>> = {
  0x8664: "x64",
  0xaa64: "arm64",
};

/**
 * Read the architecture from the PE/COFF header of a Windows executable or
 * native Node module. The caller may pass only a prefix: this validates that
 * the DOS and PE offsets are present before reading the machine field.
 */
export function windowsPeArchitecture(header: Uint8Array): WindowsPeArchitecture {
  if (header.length < 0x40 || header[0] !== 0x4d || header[1] !== 0x5a) {
    throw new Error("file is not a complete DOS/PE header");
  }
  const peOffset =
    header[0x3c] |
    (header[0x3d] << 8) |
    (header[0x3e] << 16) |
    (header[0x3f] << 24);
  if (peOffset < 0 || peOffset + 6 > header.length) {
    throw new Error("PE header offset is outside the captured file prefix");
  }
  if (
    header[peOffset] !== 0x50 ||
    header[peOffset + 1] !== 0x45 ||
    header[peOffset + 2] !== 0 ||
    header[peOffset + 3] !== 0
  ) {
    throw new Error("file has no PE signature at its declared header offset");
  }
  const machine = header[peOffset + 4] | (header[peOffset + 5] << 8);
  const architecture = PE_MACHINE_ARCHITECTURES[machine];
  if (!architecture) {
    throw new Error(`unreviewed PE machine 0x${machine.toString(16).padStart(4, "0")}`);
  }
  return architecture;
}
