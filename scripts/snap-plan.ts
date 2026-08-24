// Emit the reviewed native Snap matrix and, for tag builds, bind the tag to
// the source version and main ancestry before an expensive package build.

import { spawnSync } from "node:child_process";
import { readManifest, fail, ok, repoRoot } from "./lib/common.ts";
import { githubSnapMatrix } from "./lib/snap.ts";

const RELEASE_TAG_RE = /^v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$/;

let githubMatrix = false;
let expectedTag: string | undefined;
for (let index = 2; index < process.argv.length; index += 1) {
  const argument = process.argv[index];
  if (argument === "--github-matrix") {
    if (githubMatrix) fail("--github-matrix may appear only once");
    githubMatrix = true;
    continue;
  }
  if (argument === "--expect-tag") {
    if (expectedTag !== undefined || index + 1 >= process.argv.length) {
      fail("--expect-tag requires exactly one value");
    }
    expectedTag = process.argv[++index];
    continue;
  }
  fail(`unknown argument: ${argument}`);
}

if (expectedTag !== undefined) {
  const version = readManifest().desktopVersion;
  if (!RELEASE_TAG_RE.test(expectedTag)) {
    fail(`Snap release tag ${expectedTag} must match canonical vMAJOR.MINOR.PATCH`);
  }
  if (expectedTag !== `v${version}`) {
    fail(`Snap release tag ${expectedTag} does not match desktop version v${version}`);
  }
  const fetch = spawnSync("git", ["fetch", "origin", "main"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (fetch.status !== 0) {
    fail(`could not fetch origin/main: ${(fetch.stderr || fetch.stdout || "unknown error").trim()}`);
  }
  const ancestry = spawnSync("git", ["merge-base", "--is-ancestor", "HEAD", "FETCH_HEAD"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (ancestry.status !== 0) {
    fail(`Snap release tag ${expectedTag} is not an ancestor of origin/main`);
  }
}

if (githubMatrix) {
  process.stdout.write(JSON.stringify(githubSnapMatrix()));
} else if (expectedTag !== undefined) {
  ok(`Snap tag gate passed: ${expectedTag} is version-bound and on main`);
} else {
  for (const row of githubSnapMatrix().include) {
    console.log(`${row.id}: ${row.os} ${row.arch} -> ${row.snapArchitecture}`);
  }
  ok("Snap plan covers only native amd64 and arm64 builds");
}
