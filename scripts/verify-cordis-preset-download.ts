// Production release contract probe for Cordis preset deep links.
//
// This deliberately inspects only the direct HTTP response headers and
// immediately cancels the body. It never writes, hashes, executes, or
// installs archive bytes; the Desktop's real download path performs those
// security checks after explicit user consent.

import {
  DEFAULT_CORDIS_PRESET_SLUG,
  directZipResponseProblem,
  isValidCordisPresetSlug,
  presetDownloadUrl,
} from "./lib/cordis-preset-contract.ts";
import { fail, info, ok } from "./lib/common.ts";

const REQUEST_TIMEOUT_MS = 30_000;

function requestedSlug(): string {
  const slug = process.env.CORDIS_PRESET_SLUG ?? DEFAULT_CORDIS_PRESET_SLUG;
  if (!isValidCordisPresetSlug(slug)) {
    throw new Error(`CORDIS_PRESET_SLUG is invalid: ${JSON.stringify(slug)}`);
  }
  return slug;
}

async function main(): Promise<void> {
  const slug = requestedSlug();
  const endpoint = presetDownloadUrl(slug);
  info(`probing direct Cordis preset response: ${endpoint}`);

  const response = await fetch(endpoint, {
    redirect: "manual",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    headers: { accept: "application/zip" },
  });
  try {
    const problem = directZipResponseProblem({
      endpoint,
      status: response.status,
      statusText: response.statusText,
      contentType: response.headers.get("content-type"),
      location: response.headers.get("location"),
    });
    if (problem !== null) throw new Error(problem);
    ok(`Cordis preset ${slug} returns direct HTTP 200 application/zip`);
  } finally {
    // There is no persistent download in this probe; release the stream early.
    const body = response.body;
    if (body !== null) {
      await body.cancel().catch(() => undefined);
    }
  }
}

main().catch((error: unknown) => {
  fail(error instanceof Error ? error.message : String(error));
});
