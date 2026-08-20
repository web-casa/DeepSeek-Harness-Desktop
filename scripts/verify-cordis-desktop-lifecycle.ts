// Production Cordis Desktop lifecycle E2E.
//
// This is intentionally an explicit, Linux-only manual test. It drives a
// compiled *web-distribution* Desktop binary through Tauri's native
// WebDriver bridge, so the real bootstrap ACL and command handlers execute:
//   prepare -> stale-revision refusal -> pre-disable -> bundled pnpm add
//   (scripts disabled) -> local integrity verification -> pending -> stale
//   activation refusal -> explicit activation -> Desktop Harness restart.
//
// The production request is read-only; all local mutation lives in a newly
// created temporary DSH_HOME and pnpm store, which are removed in finally.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { delimiter, isAbsolute, join } from "node:path";
import { once } from "node:events";
import {
  marketInstallCandidateFromValue,
  parseProductionE2eConfig,
  type MarketInstallCandidate,
} from "./lib/cordis-desktop-e2e.ts";
import { fail, info, ok } from "./lib/common.ts";

const DRIVER_START_TIMEOUT_MS = 30_000;
const SESSION_START_TIMEOUT_MS = 70_000;
const COMMAND_TIMEOUT_MS = 30_000;
const INSTALL_TIMEOUT_MS = 240_000;
const RESTART_TIMEOUT_MS = 120_000;
const POLL_MS = 250;
const DRIVER_TAIL_MAX_LINES = 200;

interface Driver {
  child: ChildProcess;
  url: URL;
  tail: () => string;
}

interface Session {
  id: string;
  driver: Driver;
}

interface DesktopStatus {
  status: string;
  pid: number | null;
  distribution: string | null;
}

interface PluginRow {
  name: string;
  version: string;
  state: string;
  slug: string | null;
  entryRevision: string | null;
}

interface PluginList {
  busy: boolean;
  plugins: PluginRow[];
}

const PENDING_RECEIPT_FIELDS = [
  "slug",
  "entryRevision",
  "packageName",
  "version",
  "integrity",
  "registry",
  "tarball",
] as const;

function record(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function text(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0) throw new Error(`${label} must be text`);
  return value;
}

function pause(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function reserveLoopbackPort(): Promise<number> {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (address === null || typeof address === "string") {
    server.close();
    throw new Error("could not reserve a loopback WebDriver port");
  }
  const port = address.port;
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
  return port;
}

function processTail(child: ChildProcess): () => string {
  const lines: string[] = [];
  const append = (chunk: Buffer): void => {
    for (const line of chunk.toString("utf8").split(/\r?\n/)) {
      if (line.length === 0) continue;
      lines.push(line);
    }
    if (lines.length > DRIVER_TAIL_MAX_LINES) lines.splice(0, lines.length - DRIVER_TAIL_MAX_LINES);
  };
  child.stdout?.on("data", append);
  child.stderr?.on("data", append);
  return () => lines.join("\n");
}

function requireApplication(path: string): string {
  const resolved = realpathSync(path);
  const metadata = statSync(resolved);
  if (!metadata.isFile()) throw new Error(`Desktop application is not a regular file: ${resolved}`);
  if (process.platform !== "win32" && (metadata.mode & 0o111) === 0) {
    throw new Error(`Desktop application is not executable: ${resolved}`);
  }
  return resolved;
}

function resolveExecutable(command: string, label: string): string {
  const pathEntries = (process.env.PATH ?? "").split(delimiter).filter(Boolean);
  const candidates = isAbsolute(command)
    ? [command]
    : pathEntries.map((directory) => join(directory, command));
  for (const candidate of candidates) {
    try {
      const resolved = realpathSync(candidate);
      const metadata = statSync(resolved);
      if (!metadata.isFile()) continue;
      if (process.platform === "win32" || (metadata.mode & 0o111) !== 0) return resolved;
    } catch {
      // Try the next PATH segment. The final error identifies the requested tool.
    }
  }
  throw new Error(`${label} is not an executable file: ${command}`);
}

function startDriver(
  port: number,
  dshHome: string,
  config: ReturnType<typeof parseProductionE2eConfig>,
): Driver {
  const tauriDriver = resolveExecutable(config.tauriDriver, "tauri-driver");
  const nativeDriver = resolveExecutable(config.nativeDriver, "native WebDriver");
  const driverArgs = [
    "--native-driver",
    nativeDriver,
    "--port",
    String(port),
  ];
  const [command, args] = config.useXvfb
    ? [resolveExecutable("xvfb-run", "xvfb-run"), ["-a", tauriDriver, ...driverArgs]]
    : [tauriDriver, driverArgs];
  const child = spawn(command, args, {
    cwd: process.cwd(),
    detached: process.platform !== "win32",
    env: {
      ...process.env,
      CORDIS_RUN_API: "https://cordis.run/api/v1",
      DSH_HOME: dshHome,
      DSH_TELEMETRY_DISABLED: "1",
      pnpm_config_store_dir: join(dshHome, "pnpm-store"),
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  return {
    child,
    url: new URL(`http://127.0.0.1:${port}/`),
    tail: processTail(child),
  };
}

async function requestDriver<T>(
  driver: Driver,
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const response = await fetch(new URL(path, driver.url), {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(COMMAND_TIMEOUT_MS),
  });
  let payload: unknown;
  try {
    payload = await response.json();
  } catch (error) {
    throw new Error(`WebDriver ${method} ${path} did not return JSON: ${String(error)}`);
  }
  if (!response.ok) {
    const value = record(payload, "WebDriver error").value;
    throw new Error(
      `WebDriver ${method} ${path} failed (${response.status}): ${JSON.stringify(value)}`,
    );
  }
  return record(payload, "WebDriver response").value as T;
}

async function waitForDriver(driver: Driver): Promise<void> {
  const deadline = Date.now() + DRIVER_START_TIMEOUT_MS;
  let lastError = "not contacted";
  while (Date.now() < deadline) {
    if (driver.child.exitCode !== null) {
      throw new Error(`tauri-driver exited ${driver.child.exitCode}: ${driver.tail()}`);
    }
    try {
      const status = await requestDriver<Record<string, unknown>>(driver, "GET", "status");
      if (status.ready === true) return;
      lastError = JSON.stringify(status);
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await pause(POLL_MS);
  }
  throw new Error(`tauri-driver did not become ready: ${lastError}\n${driver.tail()}`);
}

async function createSession(driver: Driver, application: string): Promise<Session> {
  const value = await requestDriver<unknown>(driver, "POST", "session", {
    capabilities: {
      alwaysMatch: {
        browserName: "wry",
        "tauri:options": { application },
      },
    },
  });
  const id = text(record(value, "WebDriver session").sessionId, "WebDriver sessionId");
  await requestDriver(driver, "POST", `session/${id}/timeouts`, {
    script: INSTALL_TIMEOUT_MS + 60_000,
  });
  return { id, driver };
}

/**
 * WebKitWebDriver can report ready before it has enough capacity to launch a
 * WebView-backed application, especially on a freshly created Xvfb display.
 * Retry only this pre-mutation step inside a bounded window; a successful
 * session is still required before the lifecycle assertions begin.
 */
async function waitForSession(driver: Driver, application: string): Promise<Session> {
  const deadline = Date.now() + SESSION_START_TIMEOUT_MS;
  let lastError = "not attempted";
  while (Date.now() < deadline) {
    if (driver.child.exitCode !== null) {
      throw new Error(`tauri-driver exited ${driver.child.exitCode}: ${driver.tail()}`);
    }
    try {
      return await createSession(driver, application);
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await pause(POLL_MS);
  }
  throw new Error(`could not create a Desktop WebDriver session: ${lastError}\n${driver.tail()}`);
}

const INVOKE_SCRIPT = `
  const command = arguments[0];
  const payload = arguments[1];
  const done = arguments[arguments.length - 1];
  window.__TAURI_INTERNALS__.invoke(command, payload)
    .then((value) => done({ ok: true, value }))
    .catch((error) => done({ ok: false, error: String(error) }));
`;

async function invoke<T>(
  session: Session,
  command: string,
  payload: Record<string, unknown> = {},
): Promise<T> {
  const value = await requestDriver<unknown>(
    session.driver,
    "POST",
    `session/${session.id}/execute/async`,
    {
    script: INVOKE_SCRIPT,
    args: [command, payload],
    },
  );
  const result = record(value, `${command} result`);
  if (result.ok !== true) throw new Error(`${command} rejected: ${String(result.error)}`);
  return result.value as T;
}

async function expectRejected(
  session: Session,
  command: string,
  payload: Record<string, unknown>,
  expectedReason: string,
): Promise<void> {
  try {
    await invoke(session, command, payload);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!message.includes(expectedReason)) {
      throw new Error(`${command} rejected for an unexpected reason: ${message}`);
    }
    return;
  }
  throw new Error(`${command} unexpectedly accepted a stale entryRevision`);
}

function statusFromValue(value: unknown): DesktopStatus {
  const raw = record(value, "get_status result");
  const versions = record(raw.versions, "get_status versions");
  return {
    status: text(raw.status, "get_status.status"),
    pid: typeof raw.pid === "number" ? raw.pid : null,
    distribution: typeof versions.distribution === "string" ? versions.distribution : null,
  };
}

function pluginListFromValue(value: unknown): PluginList {
  const raw = record(value, "list_plugins result");
  if (!Array.isArray(raw.plugins)) throw new Error("list_plugins.plugins must be an array");
  return {
    busy: raw.busy === true,
    plugins: raw.plugins.map((value) => {
      const row = record(value, "plugin row");
      return {
        name: text(row.name, "plugin.name"),
        version: text(row.version, "plugin.version"),
        state: text(row.state, "plugin.state"),
        slug: typeof row.slug === "string" ? row.slug : null,
        entryRevision: typeof row.entryRevision === "string" ? row.entryRevision : null,
      };
    }),
  };
}

async function waitForRunning(
  session: Session,
  previousPid: number | null = null,
): Promise<DesktopStatus> {
  const deadline = Date.now() + RESTART_TIMEOUT_MS;
  let last: DesktopStatus | null = null;
  let lastError: string | null = null;
  while (Date.now() < deadline) {
    try {
      last = statusFromValue(await invoke(session, "get_status"));
      const restarted = previousPid === null || last.pid !== previousPid;
      if (last.status === "running" && last.pid !== null && restarted) {
        return last;
      }
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await pause(POLL_MS);
  }
  const diagnostic = lastError ?? JSON.stringify(last);
  throw new Error(`Desktop Harness did not become running after restart: ${diagnostic}`);
}

async function waitForPlugin(
  session: Session,
  candidate: MarketInstallCandidate,
  state: "pending" | "active",
): Promise<PluginList> {
  const deadline = Date.now() + INSTALL_TIMEOUT_MS;
  let last: PluginList | null = null;
  while (Date.now() < deadline) {
    last = pluginListFromValue(await invoke(session, "list_plugins"));
    const plugin = last.plugins.find((row) => row.name === candidate.packageName);
    if (!last.busy && plugin?.state === state) return last;
    if (state === "pending" && plugin?.state === "active") {
      throw new Error("market package became active before explicit activation");
    }
    if (!last.busy && plugin === undefined) {
      throw new Error(`Desktop market operation finished without ${candidate.packageName}`);
    }
    await pause(POLL_MS);
  }
  throw new Error(
    `timed out waiting for ${candidate.packageName} to become ${state}: ${JSON.stringify(last)}`,
  );
}

function readJson(path: string): Record<string, unknown> {
  return record(JSON.parse(readFileSync(path, "utf8")) as unknown, path);
}

function assertPendingFilesystem(dshHome: string, candidate: MarketInstallCandidate): void {
  const profile = join(dshHome, "profiles", "web");
  const manifest = readJson(join(profile, "package.json"));
  const dependencies = record(manifest.dependencies, "profile dependencies");
  if (dependencies[candidate.packageName] !== candidate.tarball) {
    throw new Error("profile dependency source differs from the reviewed tarball");
  }
  const dsh = record(manifest.dsh, "profile dsh");
  const dshProfile = record(dsh.profile, "profile dsh.profile");
  const bundles = dshProfile.bundles;
  if (!Array.isArray(bundles) || bundles.includes(candidate.packageName)) {
    throw new Error("market package became active before explicit activation");
  }
  const installed = readJson(join(profile, "node_modules", candidate.packageName, "package.json"));
  if (installed.name !== candidate.packageName || installed.version !== candidate.version) {
    throw new Error("installed package name/version differs from the reviewed source");
  }
  const installedDsh = record(installed.dsh, "installed package dsh");
  const bundle = record(installedDsh.bundle, "installed package dsh.bundle");
  if (typeof bundle.patch !== "string" || bundle.patch.length === 0) {
    throw new Error("installed package is not a DSH bundle");
  }
  const lockfile = readFileSync(join(profile, "pnpm-lock.yaml"), "utf8");
  if (!lockfile.includes(candidate.integrity)) {
    throw new Error("pnpm lockfile does not contain the reviewed integrity");
  }
  const pending = readJson(join(dshHome, ".desktop-tools", "market-pending.json"));
  const plugins = record(pending.plugins, "market pending plugins");
  const receipt = record(plugins[candidate.packageName], "market pending receipt");
  for (const key of PENDING_RECEIPT_FIELDS) {
    const expected = candidate[key];
    if (receipt[key] !== expected) {
      throw new Error(`pending receipt ${key} differs from the reviewed candidate`);
    }
  }
}

function assertActiveFilesystem(dshHome: string, candidate: MarketInstallCandidate): void {
  const profile = join(dshHome, "profiles", "web");
  const manifest = readJson(join(profile, "package.json"));
  const dsh = record(manifest.dsh, "profile dsh");
  const dshProfile = record(dsh.profile, "profile dsh.profile");
  const bundles = dshProfile.bundles;
  if (!Array.isArray(bundles) || !bundles.includes(candidate.packageName)) {
    throw new Error("explicit activation did not add the market package to bundles");
  }
  const lockfile = readFileSync(join(profile, "pnpm-lock.yaml"), "utf8");
  if (!lockfile.includes(candidate.integrity)) {
    throw new Error("lockfile integrity changed after activation");
  }
  const pending = readJson(join(dshHome, ".desktop-tools", "market-pending.json"));
  const plugins = record(pending.plugins, "market pending plugins");
  if (Object.hasOwn(plugins, candidate.packageName)) {
    throw new Error("pending marker still contains the activated package");
  }
}

async function stopDriver(driver: Driver): Promise<void> {
  if (driver.child.exitCode !== null || driver.child.pid === undefined) return;
  try {
    if (process.platform === "win32") driver.child.kill("SIGTERM");
    else process.kill(-driver.child.pid, "SIGTERM");
  } catch {
    return;
  }
  const deadline = Date.now() + 5_000;
  while (driver.child.exitCode === null && Date.now() < deadline) await pause(50);
  if (driver.child.exitCode === null) {
    try {
      if (process.platform === "win32") driver.child.kill("SIGKILL");
      else process.kill(-driver.child.pid, "SIGKILL");
    } catch {
      // The process has already exited between the check and signal.
    }
  }
}

async function main(): Promise<void> {
  const config = parseProductionE2eConfig(process.env);
  const application = requireApplication(config.application);
  const dshHome = mkdtempSync(join(tmpdir(), "dsh-desktop-cordis-production-e2e-"));
  let driver: Driver | null = null;
  let session: Session | null = null;
  try {
    const port = await reserveLoopbackPort();
    info(`starting isolated Desktop web-distribution E2E for ${config.slug}`);
    driver = startDriver(port, dshHome, config);
    await waitForDriver(driver);
    session = await waitForSession(driver, application);

    const initial = await waitForRunning(session);
    if (initial.distribution !== "web") {
      throw new Error(
        `production lifecycle E2E must use web distribution, got ${initial.distribution}`,
      );
    }
    ok("bootstrap ACL accepted get_status; isolated web Desktop Harness is running");

    const candidate = marketInstallCandidateFromValue(
      await invoke(session, "market_prepare_install", { slug: config.slug }),
      config.slug,
    );
    ok(`fresh production candidate is ${candidate.packageName}@${candidate.version}`);

    const staleRevision = `${candidate.entryRevision}-stale-e2e`;
    await expectRejected(session, "market_install_plugin", {
      slug: candidate.slug,
      entryRevision: staleRevision,
    }, "market entry changed");
    const afterStaleInstall = pluginListFromValue(await invoke(session, "list_plugins"));
    const staleInstallMutated = afterStaleInstall.plugins.some(
      (row) => row.name === candidate.packageName,
    );
    if (afterStaleInstall.busy || staleInstallMutated) {
      throw new Error("stale install revision mutated the profile");
    }
    ok("stale install revision was rejected without mutating the profile");

    await invoke(session, "market_install_plugin", {
      slug: candidate.slug,
      entryRevision: candidate.entryRevision,
    });
    await waitForPlugin(session, candidate, "pending");
    assertPendingFilesystem(dshHome, candidate);
    ok(
      "pre-disable, bundled pnpm --ignore-scripts, integrity verification, " +
        "and pending receipt passed",
    );

    await expectRejected(session, "activate_market_plugin", {
      slug: candidate.slug,
      entryRevision: staleRevision,
    }, "market entry changed");
    assertPendingFilesystem(dshHome, candidate);
    ok("stale activation revision was rejected while pending state stayed intact");

    await invoke(session, "activate_market_plugin", {
      slug: candidate.slug,
      entryRevision: candidate.entryRevision,
    });
    await waitForPlugin(session, candidate, "active");
    assertActiveFilesystem(dshHome, candidate);
    ok("explicit Activate is the only transition from pending to active");

    await invoke(session, "restart");
    const afterRestart = await waitForRunning(session, initial.pid);
    const pluginsAfterRestart = pluginListFromValue(await invoke(session, "list_plugins"));
    const restartedPlugin = pluginsAfterRestart.plugins.find(
      (row) => row.name === candidate.packageName,
    );
    if (restartedPlugin?.state !== "active") {
      throw new Error("market package is not active after Desktop Harness restart");
    }
    assertActiveFilesystem(dshHome, candidate);
    ok(
      `Desktop restarted from Harness pid ${initial.pid} to ${afterRestart.pid} ` +
        "with the package active",
    );
    console.log("\n  PASS — Cordis production Desktop IPC lifecycle E2E complete");
  } finally {
    if (session !== null) {
      await invoke(session, "shutdown").catch(() => undefined);
      await requestDriver(session.driver, "DELETE", `session/${session.id}`).catch(() => undefined);
    }
    if (driver !== null) await stopDriver(driver);
    if (existsSync(dshHome)) rmSync(dshHome, { recursive: true, force: true });
  }
}

main().catch((error: unknown) => {
  fail(error instanceof Error ? error.message : String(error));
});
