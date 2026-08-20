import { isAbsolute } from "node:path";
import { isValidCordisMarketSlug } from "./cordis-market-contract.ts";

export const PRODUCTION_CORDIS_API = "https://cordis.run/api/v1";
export const DEFAULT_PRODUCTION_E2E_SLUG = "dsh-plugin-pkgseek";

export interface CordisDesktopProductionE2eConfig {
  application: string;
  slug: string;
  tauriDriver: string;
  nativeDriver: string;
  useXvfb: boolean;
}

export interface MarketInstallCandidate {
  slug: string;
  entryRevision: string;
  packageName: string;
  version: string;
  integrity: string;
  registry: string;
  tarball: string;
}

type Environment = NodeJS.ProcessEnv;

function requiredText(value: unknown, label: string): string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`${label} must be a non-empty string`);
  }
  return value;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

/**
 * Parse the deliberate opt-in boundary for the production Desktop lifecycle
 * test. The test is intentionally Linux-only because it drives the real
 * bootstrap WebView through Tauri's native WebDriver bridge.
 */
export function parseProductionE2eConfig(
  env: Environment,
  platform: NodeJS.Platform = process.platform,
): CordisDesktopProductionE2eConfig {
  if (env.CORDIS_DESKTOP_PRODUCTION_E2E !== "1") {
    throw new Error(
      "refusing production Desktop lifecycle mutation; " +
        "set CORDIS_DESKTOP_PRODUCTION_E2E=1 explicitly",
    );
  }
  if (platform !== "linux") {
    throw new Error("the direct Tauri WebDriver lifecycle test currently supports Linux only");
  }
  const configuredApi = env.CORDIS_RUN_API;
  if (configuredApi !== undefined && configuredApi.trim() !== PRODUCTION_CORDIS_API) {
    throw new Error(
      `CORDIS_RUN_API must be exactly ${PRODUCTION_CORDIS_API} for this production test`,
    );
  }

  const application = requiredText(env.CORDIS_DESKTOP_E2E_APP, "CORDIS_DESKTOP_E2E_APP");
  if (!isAbsolute(application)) {
    throw new Error("CORDIS_DESKTOP_E2E_APP must be an absolute application path");
  }
  const slug = env.CORDIS_DESKTOP_E2E_SLUG ?? DEFAULT_PRODUCTION_E2E_SLUG;
  if (!isValidCordisMarketSlug(slug)) {
    throw new Error(`CORDIS_DESKTOP_E2E_SLUG is invalid: ${JSON.stringify(slug)}`);
  }

  return {
    application,
    slug,
    tauriDriver: env.CORDIS_DESKTOP_E2E_TAURI_DRIVER?.trim() || "tauri-driver",
    nativeDriver: env.CORDIS_DESKTOP_E2E_NATIVE_DRIVER?.trim() || "WebKitWebDriver",
    useXvfb: env.CORDIS_DESKTOP_E2E_NO_XVFB !== "1",
  };
}

/** Validate the only DTO that may cross from the WebView into mutation flow. */
export function marketInstallCandidateFromValue(
  value: unknown,
  expectedSlug: string,
): MarketInstallCandidate {
  const candidate = record(value, "market_prepare_install result");
  const parsed: MarketInstallCandidate = {
    slug: requiredText(candidate.slug, "candidate.slug"),
    entryRevision: requiredText(candidate.entryRevision, "candidate.entryRevision"),
    packageName: requiredText(candidate.packageName, "candidate.packageName"),
    version: requiredText(candidate.version, "candidate.version"),
    integrity: requiredText(candidate.integrity, "candidate.integrity"),
    registry: requiredText(candidate.registry, "candidate.registry"),
    tarball: requiredText(candidate.tarball, "candidate.tarball"),
  };
  if (parsed.slug !== expectedSlug) {
    throw new Error(`market_prepare_install returned ${parsed.slug}, expected ${expectedSlug}`);
  }
  if (!parsed.integrity.startsWith("sha512-")) {
    throw new Error("market_prepare_install did not return a sha512 integrity value");
  }
  return parsed;
}
