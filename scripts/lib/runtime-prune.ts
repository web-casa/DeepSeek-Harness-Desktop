import {
  existsSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
} from "node:fs";
import { join } from "node:path";

export type SupportedLinuxArch = "x64" | "arm64";

const KOFFI_ARCH_DIRECTORY: Readonly<Record<SupportedLinuxArch, string>> = {
  x64: "x64",
  arm64: "arm64",
};

function isSupportedLinuxArch(arch: string): arch is SupportedLinuxArch {
  return arch === "x64" || arch === "arm64";
}

export function hostUsesGlibc(): boolean {
  if (process.platform !== "linux") return false;
  const report = process.report?.getReport();
  if (typeof report === "string") return false;
  const header = (report as { header?: { glibcVersionRuntime?: unknown } } | undefined)
    ?.header;
  return typeof header?.glibcVersionRuntime === "string";
}

/**
 * Remove Koffi's unused musl native binary from a glibc Linux release tree.
 *
 * linuxdeploy scans every ELF file under AppDir, including optional variants
 * that Node will never load.  Its dependency walk therefore rejects Koffi's
 * musl binary on Ubuntu because libc.musl-* is intentionally absent.  Keep the
 * glibc sibling and remove only the reviewed, architecture-specific musl
 * directory.  Any upstream layout change aborts instead of deleting an
 * unreviewed path.
 */
export function pruneGlibcKoffiMuslVariant(
  nodeModules: string,
  arch: string,
): string {
  if (!isSupportedLinuxArch(arch)) {
    throw new Error(`unsupported glibc Linux architecture for Koffi pruning: ${arch}`);
  }

  const archDirectory = KOFFI_ARCH_DIRECTORY[arch];
  const packageName = `@koromix/koffi-linux-${arch}`;
  const packageRoot = join(nodeModules, "@koromix", `koffi-linux-${arch}`);
  const packageJsonPath = join(packageRoot, "package.json");
  const glibcBinary = join(packageRoot, `linux_${archDirectory}`, "koffi.node");
  const muslDirectory = join(packageRoot, `musl_${archDirectory}`);
  const muslBinary = join(muslDirectory, "koffi.node");

  if (!existsSync(packageJsonPath)) {
    throw new Error(`expected selected Koffi package is missing: ${packageJsonPath}`);
  }
  const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8")) as {
    name?: unknown;
  };
  if (packageJson.name !== packageName) {
    throw new Error(`unexpected Koffi package identity at ${packageJsonPath}`);
  }
  if (!existsSync(glibcBinary) || !statSync(glibcBinary).isFile()) {
    throw new Error(`expected Koffi glibc binary is missing: ${glibcBinary}`);
  }
  if (!existsSync(muslBinary) || !statSync(muslBinary).isFile()) {
    throw new Error(`expected Koffi musl binary is missing: ${muslBinary}`);
  }
  const muslEntries = readdirSync(muslDirectory).sort();
  if (muslEntries.length !== 1 || muslEntries[0] !== "koffi.node") {
    throw new Error(
      `refusing to prune changed Koffi musl layout: ${muslDirectory} contains ${muslEntries.join(", ")}`,
    );
  }

  rmSync(muslDirectory, { recursive: true });
  return muslDirectory;
}
