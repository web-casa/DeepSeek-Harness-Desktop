/** Pure arbitration for security-sensitive install confirmation surfaces. */

export interface PluginRequestIdentity {
  name: string;
  source: string;
  slug: string;
}

export interface InstallSurfaceSnapshot {
  plugin: PluginRequestIdentity | null;
  remotePresetRequestId: string | null;
  localPresetPreview: boolean;
  marketConfirmation: boolean;
  marketPreparing: boolean;
}

export type InstallConflict = "plugin" | "preset" | "market";
export interface InstallArbitration {
  action: "accept" | "keep-current" | "dismiss-incoming";
  conflict?: InstallConflict;
  notify: boolean;
}

function samePlugin(a: PluginRequestIdentity, b: PluginRequestIdentity): boolean {
  return a.name === b.name && a.source === b.source && a.slug === b.slug;
}

export function arbitratePluginRequest(
  snapshot: InstallSurfaceSnapshot,
  incoming: PluginRequestIdentity,
): InstallArbitration {
  if (snapshot.plugin) {
    return {
      action: "keep-current",
      conflict: "plugin",
      notify: !samePlugin(snapshot.plugin, incoming),
    };
  }
  if (snapshot.marketConfirmation || snapshot.marketPreparing) {
    return { action: "dismiss-incoming", conflict: "market", notify: true };
  }
  if (snapshot.remotePresetRequestId || snapshot.localPresetPreview) {
    return { action: "dismiss-incoming", conflict: "preset", notify: true };
  }
  return { action: "accept", notify: false };
}

export function arbitrateRemotePresetRequest(
  snapshot: InstallSurfaceSnapshot,
  incomingRequestId: string,
): InstallArbitration {
  if (snapshot.plugin || snapshot.localPresetPreview) {
    return { action: "dismiss-incoming", conflict: "plugin", notify: true };
  }
  if (snapshot.marketConfirmation || snapshot.marketPreparing) {
    return { action: "dismiss-incoming", conflict: "market", notify: true };
  }
  if (
    snapshot.remotePresetRequestId &&
    snapshot.remotePresetRequestId !== incomingRequestId
  ) {
    return { action: "dismiss-incoming", conflict: "preset", notify: true };
  }
  return { action: "accept", notify: false };
}
