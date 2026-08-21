export interface PluginCompletionSnapshot {
  frontendBusy: boolean;
  backendBusy: boolean;
  doneExpected: boolean;
}

export interface PluginCompletionDecision {
  doneExpected: boolean;
  missedDone: boolean;
}

/**
 * Reconcile the live Tauri event stream with the durable backend busy flag.
 * Synchronous mutations (activation) deliberately expect no plugin-done;
 * spawned pnpm/dsh operations do. A freshly mounted webview cannot infer the
 * operation kind from the shared backend busy flag (activation and recovery
 * use the same single-flight gate), so it adopts busy state without inventing
 * an expected event. Polling still clears that state when the backend ends.
 */
export function reconcilePluginCompletion({
  frontendBusy,
  backendBusy,
  doneExpected,
}: PluginCompletionSnapshot): PluginCompletionDecision {
  return {
    missedDone: frontendBusy && !backendBusy && doneExpected,
    doneExpected: backendBusy ? doneExpected : false,
  };
}
