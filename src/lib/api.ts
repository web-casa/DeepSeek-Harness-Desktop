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
}

export const getStatus = (): Promise<StatusPayload> => invoke("get_status");
export const getLogs = (): Promise<[string, string][]> => invoke("get_logs");
export const getVersions = (): Promise<Versions> => invoke("get_versions");
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
