// Pure logic for verify-bundle.ts — separated so unit tests can import it
// without executing the CLI body.

/// Pure discriminator for the xattr probe: `null` means "could not determine"
/// (tool missing/errored) and callers must fail closed — a missing xattr must
/// never be mistaken for "no quarantine attribute".
export function quarantinePresent(
  status: number | null,
  error?: Error,
): boolean | null {
  if (status === null || error) return null;
  return status === 0;
}

/// Parse `7z l -slt` output into entry paths (forward slashes). The listing
/// opens with an archive-level header block that must not be treated as a
/// content entry: only collect `Path = ` lines after the first long dash
/// separator.
export function parseSltListing(text: string): string[] {
  const entries: string[] = [];
  let inEntries = false;
  for (const line of text.split(/\r?\n/)) {
    if (!inEntries) {
      if (/^-{10,}$/.test(line)) inEntries = true;
      continue;
    }
    if (line.startsWith("Path = ")) {
      entries.push(line.slice("Path = ".length).replace(/\\/g, "/"));
    }
  }
  return entries;
}
