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
