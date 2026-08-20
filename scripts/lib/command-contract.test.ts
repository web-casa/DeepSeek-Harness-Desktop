import assert from "node:assert/strict";
import { test } from "node:test";
import {
  bootstrapCommands,
  commandContractProblems,
  frontendInvokeCommands,
  handlerCommands,
  manifestCommands,
  rustDelimitedBody,
} from "./command-contract.ts";

const main = `
  .invoke_handler(tauri::generate_handler![
    commands::get_status,
    /* a nested expression must not close the macro: [commands::not_real] */
    diagnostics::export_diagnostics,
  ]);
`;
const build = `
  let manifest = tauri_build::AppManifest::new().commands(&[
    "get_status",
    // comment [ignored] "not_real"
    "export_diagnostics",
  ]);
`;
const bootstrap = JSON.stringify({
  identifier: "bootstrap",
  windows: ["bootstrap"],
  permissions: ["core:default", "allow-get-status", "dialog:allow-save", "allow-export-diagnostics"],
});
const harness = JSON.stringify({ identifier: "harness", windows: ["harness"], permissions: [] });

test("balanced Rust extraction tolerates comments and nested delimiters", () => {
  assert.match(rustDelimitedBody(main, "tauri::generate_handler!"), /commands::get_status/);
  assert.deepEqual(handlerCommands(main), ["export_diagnostics", "get_status"]);
  assert.deepEqual(manifestCommands(build), ["export_diagnostics", "get_status"]);
});

test("capability normalization ignores plugin/core permissions", () => {
  assert.deepEqual(bootstrapCommands(bootstrap), ["export_diagnostics", "get_status"]);
});

test("aligned command contract has no problems", () => {
  assert.deepEqual(commandContractProblems({ main, build, bootstrap, harness }), []);
});

test("drift and Harness privilege growth fail with actionable differences", () => {
  const unsafeHarness = JSON.stringify({
    identifier: "harness",
    windows: ["harness", "*"],
    permissions: ["core:default"],
  });
  const problems = commandContractProblems({
    main,
    build: build.replace('    "export_diagnostics",\n', ""),
    bootstrap: bootstrap.replace("allow-export-diagnostics", "allow-quit-app"),
    harness: unsafeHarness,
  });
  assert.deepEqual(problems, [
    "build.rs AppManifest missing: export_diagnostics",
    "bootstrap capability missing: export_diagnostics",
    "bootstrap capability extra: quit_app",
    'Harness capability windows must equal ["harness"]',
    "Harness capability permissions must remain empty",
  ]);
});

test("frontend invokes may be a subset but cannot name an unregistered command", () => {
  assert.deepEqual(
    frontendInvokeCommands(
      'invoke("get_status"); invoke("get_status"); // invoke("commented")\nconst text = \'invoke("in_string")\';',
    ),
    ["get_status"],
  );
  const problems = commandContractProblems({
    main,
    build,
    bootstrap,
    harness,
    frontend: 'invoke("get_status"); invoke("root_shell")',
  });
  assert.deepEqual(problems, ["frontend invokes unregistered commands: root_shell"]);
});

test("malformed Rust and capability inputs fail closed", () => {
  assert.throws(() => handlerCommands("tauri::generate_handler![commands::x"), /unterminated/);
  assert.deepEqual(
    commandContractProblems({ main, build, bootstrap: "[]", harness }),
    ["bootstrap capability is not valid JSON: root must be an object"],
  );
});
