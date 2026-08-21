import { test } from "node:test";
import assert from "node:assert/strict";
import {
  arbitratePluginRequest,
  arbitrateRemotePresetRequest,
  type InstallSurfaceSnapshot,
} from "../../src/lib/install-arbitration.ts";

const empty: InstallSurfaceSnapshot = {
  plugin: null,
  remotePresetRequestId: null,
  localPresetPreview: false,
  marketConfirmation: false,
  marketPreparing: false,
};
const plugin = { name: "Claimed name", source: "dsh://plugin/example", slug: "example" };

test("an existing plugin confirmation can never be replaced by another deep link", () => {
  assert.deepEqual(arbitratePluginRequest({ ...empty, plugin }, plugin), {
    action: "keep-current",
    conflict: "plugin",
    notify: false,
  });
  assert.deepEqual(
    arbitratePluginRequest(
      { ...empty, plugin },
      { name: "Attacker", source: "dsh://plugin/attacker", slug: "attacker" },
    ),
    { action: "keep-current", conflict: "plugin", notify: true },
  );
});

test("market and preset confirmations dismiss an incoming plugin request", () => {
  for (const occupied of [
    { ...empty, marketPreparing: true },
    { ...empty, marketConfirmation: true },
    { ...empty, remotePresetRequestId: "preset-1" },
    { ...empty, localPresetPreview: true },
  ]) {
    assert.equal(arbitratePluginRequest(occupied, plugin).action, "dismiss-incoming");
  }
});

test("remote preset arbitration dismisses only conflicts and accepts idempotent updates", () => {
  assert.equal(
    arbitrateRemotePresetRequest({ ...empty, plugin }, "preset-2").action,
    "dismiss-incoming",
  );
  assert.equal(
    arbitrateRemotePresetRequest({ ...empty, remotePresetRequestId: "preset-1" }, "preset-2")
      .action,
    "dismiss-incoming",
  );
  assert.equal(
    arbitrateRemotePresetRequest({ ...empty, remotePresetRequestId: "preset-1" }, "preset-1")
      .action,
    "accept",
  );
  assert.equal(arbitrateRemotePresetRequest(empty, "preset-1").action, "accept");
});
