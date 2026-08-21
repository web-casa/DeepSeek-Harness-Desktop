import assert from "node:assert/strict";
import { test } from "node:test";
import { reconcilePluginCompletion } from "../../src/lib/plugin-completion.ts";

test("an asynchronous operation reports a missed completion event", () => {
  assert.deepEqual(
    reconcilePluginCompletion({
      frontendBusy: true,
      backendBusy: false,
      doneExpected: true,
    }),
    { doneExpected: false, missedDone: true },
  );
});

test("a synchronous activation does not synthesize a missing event error", () => {
  assert.deepEqual(
    reconcilePluginCompletion({
      frontendBusy: true,
      backendBusy: false,
      doneExpected: false,
    }),
    { doneExpected: false, missedDone: false },
  );
});

test("a reloaded webview adopts busy state without guessing its operation kind", () => {
  assert.deepEqual(
    reconcilePluginCompletion({
      frontendBusy: false,
      backendBusy: true,
      doneExpected: false,
    }),
    { doneExpected: false, missedDone: false },
  );
});

test("an operation that remains busy preserves its completion contract", () => {
  assert.deepEqual(
    reconcilePluginCompletion({
      frontendBusy: true,
      backendBusy: true,
      doneExpected: false,
    }),
    { doneExpected: false, missedDone: false },
  );
});
