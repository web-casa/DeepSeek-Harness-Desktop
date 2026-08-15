import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Status =
  | "idle"
  | "starting"
  | "running"
  | "stopping"
  | "stopped"
  | "crashed";

export interface Versions {
  desktop: string;
  harness: string;
  node: string;
  sidecar: string;
}

export interface StatusPayload {
  status: Status;
  url: string | null;
  pid: number | null;
  lastError: string | null;
  versions?: Versions;
  dshHome?: string | null;
}

export const getStatus = (): Promise<StatusPayload> => invoke("get_status");
export const getLogs = (): Promise<[string, string][]> => invoke("get_logs");
export const getVersions = (): Promise<Versions> => invoke("get_versions");
export const getDiagnostics = (): Promise<Record<string, unknown>> =>
  invoke("get_diagnostics");
export const restart = (): Promise<void> => invoke("restart");
export const shutdown = (): Promise<void> => invoke("shutdown");
export const openHarness = (): Promise<void> => invoke("open_harness");

export async function onEvent(
  handler: (payload: StatusPayload) => void,
): Promise<() => void> {
  const unlisten = await listen<StatusPayload>("harness-event", (event) => {
    handler(event.payload);
  });
  return unlisten;
}

export interface UpdateInfo {
  available: boolean;
  version?: string;
  notes?: string;
  unsupported?: boolean;
}

export const checkUpdate = (): Promise<UpdateInfo> => invoke("check_update");
export const installUpdateAndRestart = (): Promise<void> =>
  invoke("install_update_and_restart");
export const exportDiagnostics = (): Promise<void> => invoke("export_diagnostics");
export const quitApp = (): Promise<void> => invoke("quit_app");
