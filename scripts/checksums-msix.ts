import { createHash } from "node:crypto";
import { createReadStream, existsSync, readdirSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";
import { repoRoot, fail, ok } from "./lib/common.ts";

const archIndex = process.argv.indexOf("--arch");
const arch = archIndex >= 0 ? process.argv[archIndex + 1] : undefined;
if (arch !== "x64" && arch !== "arm64") fail("--arch must be x64 or arm64");
const directory = join(repoRoot, "target", "msix");
if (!existsSync(directory)) fail(`MSIX directory missing: ${directory}`);
const candidates = readdirSync(directory)
  .filter((name) => name.toLowerCase().endsWith(".msix"))
  .filter((name) => name.includes(`_${arch}`) || name.includes(`-${arch}`));
if (candidates.length !== 1) {
  fail(`expected one ${arch} MSIX, found ${candidates.join(", ") || "none"}`);
}
const artifact = join(directory, candidates[0]);
const hash = createHash("sha256");
for await (const chunk of createReadStream(artifact)) hash.update(chunk);
const digest = hash.digest("hex");
writeFileSync(`${artifact}.sha256`, `${digest}  ${basename(artifact)}\n`);
ok(`${basename(artifact)} sha256 → ${digest}`);
