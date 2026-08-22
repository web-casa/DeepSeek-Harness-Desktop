import {
  BUNDLE_SPECS,
  NATIVE_RELEASE_TARGETS,
  publicArtifactsFor,
  type PublicBundle,
} from "./release-artifacts.ts";
import {
  WINDOWS_WIX_INSTALLER_LOCALES,
  type WindowsWixInstallerLocale,
  wixInstallerLocaleFromMsiName,
} from "./windows-installer-locales.ts";

export function classifyPublicInstaller(name: string): PublicBundle | null {
  for (const [bundle, spec] of Object.entries(BUNDLE_SPECS) as [PublicBundle, (typeof BUNDLE_SPECS)[PublicBundle]][]) {
    if (name.endsWith(spec.suffix)) return bundle;
  }
  return null;
}

export function expectedPublicBundleCounts(): Record<PublicBundle, number> {
  const counts = Object.fromEntries(
    (Object.keys(BUNDLE_SPECS) as PublicBundle[]).map((bundle) => [bundle, 0]),
  ) as Record<PublicBundle, number>;
  for (const target of NATIVE_RELEASE_TARGETS) {
    for (const artifact of publicArtifactsFor(target)) counts[artifact.bundle] += 1;
  }
  return counts;
}

export function expectedMsiInstallerLocaleCounts(): Record<WindowsWixInstallerLocale, number> {
  const counts = Object.fromEntries(
    WINDOWS_WIX_INSTALLER_LOCALES.map((locale) => [locale, 0]),
  ) as Record<WindowsWixInstallerLocale, number>;
  for (const target of NATIVE_RELEASE_TARGETS) {
    for (const artifact of publicArtifactsFor(target)) {
      if (artifact.bundle === "msi" && artifact.installerLocale) {
        counts[artifact.installerLocale] += 1;
      }
    }
  }
  return counts;
}

export function msiLocaleInventoryProblems(msiNames: readonly string[]): string[] {
  const expected = expectedMsiInstallerLocaleCounts();
  const counts = Object.fromEntries(
    WINDOWS_WIX_INSTALLER_LOCALES.map((locale) => [locale, 0]),
  ) as Record<WindowsWixInstallerLocale, number>;
  const problems: string[] = [];
  for (const name of msiNames) {
    const locale = wixInstallerLocaleFromMsiName(name);
    if (!locale) {
      problems.push(`MSI filename lacks a reviewed WiX locale suffix: ${name}`);
      continue;
    }
    counts[locale] += 1;
  }
  for (const locale of WINDOWS_WIX_INSTALLER_LOCALES) {
    if (counts[locale] !== expected[locale]) {
      problems.push(`release inventory MSI ${locale} count ${counts[locale]} != expected ${expected[locale]}`);
    }
  }
  return problems;
}

export function expectedUpdaterSignatureCount(): number {
  return NATIVE_RELEASE_TARGETS.reduce(
    (count, target) =>
      count + (target.updaterSignature?.publish ? 1 : 0),
    0,
  );
}
