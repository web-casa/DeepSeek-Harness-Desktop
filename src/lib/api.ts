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

export interface PresetPreview {
  id: string;
  files: [string, number][];
  warnings: string[];
}

export type PresetIssueKind = "broken" | "unsafe" | "info";

export interface PresetIssue {
  kind: PresetIssueKind;
  detail: string;
}

export interface PresetRow {
  id: string;
  issues: PresetIssue[];
}

export const listUserPresets = (): Promise<PresetRow[]> => invoke("list_user_presets");
export const previewPreset = (): Promise<PresetPreview> => invoke("preview_preset");
export const importPreset = (): Promise<string> => invoke("import_preset");
export const exportPreset = (id: string): Promise<void> =>
  invoke("export_preset", { id });
export const deletePreset = (id: string): Promise<void> =>
  invoke("delete_preset", { id });

export async function onUpdateProgress(
  handler: (p: { downloaded: number; total: number | null }) => void,
): Promise<() => void> {
  const unlisten = await listen<{ downloaded: number; total: number | null }>(
    "update-progress",
    (event) => handler(event.payload),
  );
  return unlisten;
}

export interface PluginEntry {
  name: string;
  version: string;
}

export interface PluginList {
  plugins: PluginEntry[];
  busy: boolean;
}

export interface PluginLogLine {
  stream: string;
  line: string;
}

export interface PluginDone {
  exit: number | null;
  tail: string;
}

export const listPlugins = (): Promise<PluginList> => invoke("list_plugins");
export const installPlugin = (name: string): Promise<void> =>
  invoke("install_plugin", { name });
export const uninstallPlugin = (name: string): Promise<void> =>
  invoke("uninstall_plugin", { name });
export const cancelPluginOp = (): Promise<void> => invoke("cancel_plugin_op");

export async function onPluginLog(
  handler: (lines: PluginLogLine[]) => void,
): Promise<() => void> {
  const unlisten = await listen<PluginLogLine[]>("plugin-log", (event) => {
    handler(event.payload);
  });
  return unlisten;
}

export async function onPluginDone(
  handler: (payload: PluginDone) => void,
): Promise<() => void> {
  const unlisten = await listen<PluginDone>("plugin-done", (event) => {
    handler(event.payload);
  });
  return unlisten;
}
