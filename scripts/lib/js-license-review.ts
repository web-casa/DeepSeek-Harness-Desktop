const DENIED_LICENSES = [
  "GPL-2.0-only",
  "GPL-3.0-only",
  "AGPL-3.0-only",
  "LGPL-2.1-only",
  "LGPL-3.0-only",
] as const;

type PackageLicense = {
  name: string;
  version: string;
  license: string;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function licenseText(value: unknown): string | undefined {
  if (typeof value === "string") return value;
  if (isRecord(value) && typeof value.type === "string") return value.type;
  return undefined;
}

function containsDeniedLicense(expression: string): string | undefined {
  return DENIED_LICENSES.find((denied) => {
    const escaped = denied.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    return new RegExp(`(^|[^A-Za-z0-9.-])${escaped}($|[^A-Za-z0-9.-])`).test(
      expression,
    );
  });
}

function parsePnpmReport(report: unknown): PackageLicense[] {
  if (!isRecord(report)) throw new Error("pnpm license report must be an object");
  const packages: PackageLicense[] = [];
  for (const [license, entries] of Object.entries(report)) {
    if (!Array.isArray(entries)) {
      throw new Error(`pnpm license group ${license} must be an array`);
    }
    for (const entry of entries) {
      if (!isRecord(entry) || typeof entry.name !== "string") {
        throw new Error(`pnpm license group ${license} contains an invalid package`);
      }
      const versions = Array.isArray(entry.versions)
        ? entry.versions.filter(
            (version): version is string => typeof version === "string",
          )
        : [];
      packages.push({
        name: entry.name,
        version: versions.join(",") || "unknown",
        license: licenseText(entry.license) ?? license,
      });
    }
  }
  return packages;
}

function parseNpmQuery(report: unknown): PackageLicense[] {
  if (!Array.isArray(report)) throw new Error("npm query report must be an array");
  const packages: PackageLicense[] = [];
  for (const entry of report) {
    if (!isRecord(entry)) throw new Error("npm query contains an invalid package");
    // npm includes the private workspace root; it is not an installed dependency.
    if (entry.location === "") continue;
    if (typeof entry.name !== "string") throw new Error("npm package name is missing");
    const license = licenseText(entry.license);
    if (!license) continue;
    packages.push({
      name: entry.name,
      version: typeof entry.version === "string" ? entry.version : "unknown",
      license,
    });
  }
  return packages;
}

export type JavaScriptLicenseReportFormat = "pnpm" | "npm-query";

export function reviewJavaScriptLicenses(
  report: unknown,
  format: JavaScriptLicenseReportFormat,
): { checked: number; violations: string[] } {
  const packages = format === "pnpm" ? parsePnpmReport(report) : parseNpmQuery(report);
  const violations = packages.flatMap((pkg) => {
    const denied = containsDeniedLicense(pkg.license);
    return denied
      ? [`${pkg.name}@${pkg.version}: ${pkg.license} (denies ${denied})`]
      : [];
  });
  return { checked: packages.length, violations };
}
