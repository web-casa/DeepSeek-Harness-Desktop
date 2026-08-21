import { chmodSync, mkdirSync, writeFileSync } from "node:fs";
import { delimiter, join } from "node:path";

function posixShellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

/** Mirrors `plugins::pnpm_shim_script` for runtime smoke fixtures. */
export function pluginShimScript(node: string, pnpmCjs: string): string {
  return `#!/bin/sh\nexec ${posixShellQuote(node)} ${posixShellQuote(pnpmCjs)} "$@"\n`;
}

/** Mirrors `plugins::pnpm_shim_cmd` for runtime smoke fixtures. */
export function pluginShimCmd(node: string, pnpmCjs: string): string {
  const escapedNode = node.replaceAll("%", "%%");
  const escapedPnpm = pnpmCjs.replaceAll("%", "%%");
  return `@echo off\nsetlocal DisableDelayedExpansion\n"${escapedNode}" "${escapedPnpm}" %*\n`;
}

export function pluginPath(toolsDir: string, inheritedPath: string | undefined): string {
  return inheritedPath ? `${toolsDir}${delimiter}${inheritedPath}` : toolsDir;
}

/** Mirrors the direct, scripts-disabled Desktop uninstall invocation. */
export function pluginRemoveArgs(
  packageName: string,
  configHome: string,
  storeDir: string | undefined,
): string[] {
  const npmrc = join(configHome, ".npmrc");
  const args = [
    "remove",
    packageName,
    "--ignore-workspace",
    "--global=false",
    "--node-linker=hoisted",
    "--config.auto-install-peers=false",
    "--package-import-method=copy",
    "--virtual-store-dir=node_modules/.pnpm",
    "--yes",
    "--reporter=append-only",
    "--config.ignore-scripts=true",
    "--config.ignore-pnpmfile=true",
    "--config.enable-global-virtual-store=false",
    "--config.verify-store-integrity=true",
    "--config.strict-store-pkg-content-check=true",
    `--config.config-dir=${configHome}`,
    `--config.userconfig=${npmrc}`,
    `--config.globalconfig=${npmrc}`,
  ];
  if (storeDir) args.push(`--store-dir=${storeDir}`);
  return args;
}

/** Mirrors the direct, pending-only Desktop market installation invocation. */
export function pluginMarketAddArgs(
  tarball: string,
  registry: string,
  configHome: string,
  storeDir: string | undefined,
): string[] {
  const npmrc = join(configHome, ".npmrc");
  const args = [
    "add",
    tarball,
    "--ignore-workspace",
    "--global=false",
    "--node-linker=hoisted",
    "--config.auto-install-peers=false",
    "--package-import-method=copy",
    "--virtual-store-dir=node_modules/.pnpm",
    "--yes",
    "--reporter=append-only",
    "--ignore-scripts",
    "--config.ignore-pnpmfile=true",
    "--config.enable-global-virtual-store=false",
    "--verify-store-integrity",
    "--config.strict-store-pkg-content-check=true",
    `--config.config-dir=${configHome}`,
    `--config.userconfig=${npmrc}`,
    `--config.globalconfig=${npmrc}`,
    "--save-exact",
    `--registry=${registry}`,
  ];
  if (storeDir) args.push(`--store-dir=${storeDir}`);
  return args;
}

export function writePluginShims(toolsDir: string, node: string, pnpmCjs: string): void {
  mkdirSync(toolsDir, { recursive: true });
  const unix = join(toolsDir, "pnpm");
  writeFileSync(unix, pluginShimScript(node, pnpmCjs));
  writeFileSync(join(toolsDir, "pnpm.cmd"), pluginShimCmd(node, pnpmCjs));
  if (process.platform !== "win32") chmodSync(unix, 0o700);
}
