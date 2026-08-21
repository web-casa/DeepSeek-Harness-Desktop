import { fail, ok } from "./lib/common.ts";
import {
  reviewJavaScriptLicenses,
  type JavaScriptLicenseReportFormat,
} from "./lib/js-license-review.ts";

function parseFormat(): JavaScriptLicenseReportFormat {
  const index = process.argv.indexOf("--format");
  const value = index >= 0 ? process.argv[index + 1] : undefined;
  if (value !== "pnpm" && value !== "npm-query") {
    fail("usage: verify-js-licenses.ts --format <pnpm|npm-query>");
  }
  return value;
}

const chunks: string[] = [];
let length = 0;
process.stdin.setEncoding("utf8");
for await (const chunk of process.stdin) {
  length += chunk.length;
  if (length > 32 * 1024 * 1024) fail("license report exceeds 32 MiB");
  chunks.push(chunk);
}

let report: unknown;
try {
  report = JSON.parse(chunks.join(""));
} catch (error) {
  fail(`license report is not valid JSON: ${String(error)}`);
}

const result = reviewJavaScriptLicenses(report, parseFormat());
if (result.violations.length > 0) {
  fail(`disallowed JavaScript licenses:\n- ${result.violations.join("\n- ")}`);
}
ok(`JavaScript license gate checked ${result.checked} installed packages`);
