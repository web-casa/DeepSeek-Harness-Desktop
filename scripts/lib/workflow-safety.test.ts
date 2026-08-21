import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const workflow = readFileSync(
  new URL("../../.github/workflows/test.yml", import.meta.url),
  "utf8",
);

test("macOS shell path gate validates event SHAs and fails closed", () => {
  const start = workflow.indexOf("      - name: Detect macOS shell changes");
  const end = workflow.indexOf("      # The exact P0 acceptance chain", start);
  assert.ok(start >= 0 && end > start, "macOS path-gate step missing");
  const step = workflow.slice(start, end);
  const shellStart = step.indexOf("        run: |");
  assert.ok(shellStart >= 0, "macOS path-gate shell body missing");
  const shell = step.slice(shellStart);

  assert.match(step, /BASE_SHA: \$\{\{ github\.event\.pull_request\.base\.sha/);
  assert.match(step, /HEAD_SHA: \$\{\{ github\.sha \}\}/);
  assert.doesNotMatch(shell, /\$\{\{/);
  assert.ok(shell.includes("sha_pattern='^[0-9a-fA-F]{40}$'"));
  assert.ok(shell.includes('git cat-file -e "${BASE_SHA}^{commit}"'));
  assert.ok(shell.includes('git cat-file -e "${HEAD_SHA}^{commit}"'));
  assert.ok(shell.includes('git diff --quiet "$BASE_SHA" "$HEAD_SHA"'));
  assert.ok(shell.includes("printf 'run=true\\n' >> \"$GITHUB_OUTPUT\""));
  assert.ok(shell.includes("run_check=true"));
});

test("native macOS compile is path-gated and bounded", () => {
  assert.match(workflow, /workflow_dispatch:/);
  assert.match(workflow, /timeout-minutes: 20/);
  assert.match(
    workflow,
    /if: runner\.os == 'macOS' && steps\.macos-shell\.outputs\.run == 'true'\n        timeout-minutes: 10\n        run: cargo check --locked --manifest-path src-tauri\/Cargo\.toml --all-targets/,
  );
});
