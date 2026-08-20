// Resolve optional macOS signing and notarization credentials without logging
// any secret values. ASC API-key auth is preferred when complete; the existing
// Apple ID + app-specific-password path remains a supported fallback.

import { appendFileSync } from "node:fs";
import { fail, info, ok } from "./lib/common.ts";
import { resolveMacosSigningConfiguration } from "./lib/macos-notarization.ts";

const configuration = resolveMacosSigningConfiguration(process.env);
if (configuration.problems.length > 0) fail(configuration.problems.join("\n"));

const values = configuration.configured
  ? {
      APPLE_SIGNING_CONFIGURED: "1",
      APPLE_NOTARIZATION_PROVIDER: configuration.provider!,
    }
  : { APPLE_SIGNING_CONFIGURED: "0", APPLE_NOTARIZATION_PROVIDER: "" };

const githubEnv = process.env.GITHUB_ENV;
if (githubEnv) {
  if (/\r|\n/.test(githubEnv)) fail("GITHUB_ENV contains an unsafe newline");
  appendFileSync(
    githubEnv,
    Object.entries(values)
      .map(([name, value]) => `${name}=${value}\n`)
      .join(""),
  );
} else {
  for (const [name, value] of Object.entries(values)) console.log(`${name}=${value}`);
}

if (configuration.configured) {
  ok(`macOS signing configured; notarization provider: ${configuration.provider}`);
} else {
  info("Apple signing is not configured; building an intentionally unsigned DMG");
}
