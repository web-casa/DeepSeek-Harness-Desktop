// Pure contract helpers for the public Cordis preset download endpoint.
//
// The Desktop deep-link boundary accepts only this canonical HTTPS route and
// rejects redirects. Keep this independent of market DTOs so a release can
// verify the production endpoint without installing a preset.

export const CORDIS_PRESET_ORIGIN = "https://cordis.run";
export const DEFAULT_CORDIS_PRESET_SLUG = "code";

const PRESET_SLUG_RE = /^[a-z0-9][a-z0-9-]*$/;

export function isValidCordisPresetSlug(value: string): boolean {
  return PRESET_SLUG_RE.test(value);
}

export function presetDownloadUrl(slug: string): URL {
  if (!isValidCordisPresetSlug(slug)) {
    throw new Error(`invalid Cordis preset slug: ${JSON.stringify(slug)}`);
  }
  return new URL(`/api/presets/${slug}/download`, CORDIS_PRESET_ORIGIN);
}

export function isZipContentType(contentType: string | null): boolean {
  return contentType?.split(";", 1)[0]?.trim().toLowerCase() === "application/zip";
}

export function directZipResponseProblem(args: {
  endpoint: URL;
  status: number;
  statusText: string;
  contentType: string | null;
  location: string | null;
}): string | null {
  const { endpoint, status, statusText, contentType, location } = args;
  if (status !== 200) {
    const redirectHint = location === null ? "" : ` (Location: ${JSON.stringify(location)})`;
    return (
      `expected direct HTTP 200 from ${endpoint}, got ${status}` +
      `${statusText ? ` ${statusText}` : ""}${redirectHint}; redirects are rejected by the Desktop preset contract`
    );
  }
  if (location !== null) {
    return `expected no redirect Location from ${endpoint}, got ${JSON.stringify(location)}`;
  }
  if (!isZipContentType(contentType)) {
    return `expected Content-Type application/zip from ${endpoint}, got ${JSON.stringify(contentType)}`;
  }
  return null;
}
