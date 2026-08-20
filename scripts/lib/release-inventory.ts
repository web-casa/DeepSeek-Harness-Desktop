import { BUNDLE_SPECS, NATIVE_RELEASE_TARGETS, type PublicBundle } from "./release-artifacts.ts";

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
    for (const bundle of target.bundles) counts[bundle] += 1;
  }
  return counts;
}

export function expectedUpdaterSignatureCount(): number {
  return NATIVE_RELEASE_TARGETS.reduce(
    (count, target) =>
      count + (target.updaterSignature?.publish ? 1 : 0),
    0,
  );
}
