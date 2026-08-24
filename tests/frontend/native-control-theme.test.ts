import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const css = readFileSync(new URL("../../src/app.css", import.meta.url), "utf8");
const controller = readFileSync(new URL("../../src/App.svelte", import.meta.url), "utf8");

test("controller native selects preserve readable dark contrast on WebKitGTK", () => {
  assert.match(css, /^:root\s*\{[^}]*color-scheme:\s*dark;/m);
  assert.match(
    css,
    /^select\s*\{[^}]*color-scheme:\s*dark;[^}]*color:\s*var\(--text\);[^}]*background-color:\s*var\(--panel-2\);/m,
  );
  assert.match(
    css,
    /^select option\s*\{[^}]*color:\s*var\(--text\);[^}]*background-color:\s*var\(--panel-2\);/m,
  );
  assert.match(
    controller,
    /<select\b[^>]*value=\{localePreference\}[^>]*aria-label=\{t\("locale\.label"\)\}[^>]*>/,
    "the language selector must inherit the native-select contract",
  );
  assert.match(
    controller,
    /<select\b[^>]*class="plugin-input"[^>]*bind:value=\{marketCategory\}[^>]*aria-label=\{t\("market\.categoryLabel"\)\}[^>]*>/,
    "the marketplace category selector must inherit the native-select contract",
  );
});
