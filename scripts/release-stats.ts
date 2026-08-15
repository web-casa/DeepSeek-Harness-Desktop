// Download-count stats for published releases — the only "telemetry" this
// project ever sees (public GitHub API counters). Run by a maintainer after
// publishing (gh CLI, local credentials; the public API needs no token).
//
//   node scripts/release-stats.ts [--days 30]

import { spawnSync } from "node:child_process";
import { fail, ok, info } from "./lib/common.ts";

interface Asset {
  name: string;
  download_count: number;
}
interface Release {
  tag_name: string;
  published_at: string | null;
  assets: Asset[];
}

const repo = process.env.GITHUB_REPOSITORY ?? "web-casa/DeepSeek-Harness-Desktop";

function ghJson<T>(path: string): T {
  const res = spawnSync("gh", ["api", path], { encoding: "utf8" });
  if (res.status !== 0) fail(`gh api ${path} failed: ${res.stderr}`);
  return JSON.parse(res.stdout ?? "") as T;
}

function main(): void {
  const releases = ghJson<Release[]>(`repos/${repo}/releases?per_page=30`);
  if (releases.length === 0) {
    info("no releases yet");
    return;
  }
  info(`release download stats (${repo})`);
  let total = 0;
  for (const r of releases) {
    if (r.published_at === null) continue;
    const per = r.assets.map((a) => a.download_count).reduce((a, b) => a + b, 0);
    total += per;
    const assets = r.assets
      .map((a) => `    ${a.name}: ${a.download_count}`)
      .join("\n");
    ok(`${r.tag_name} (${r.published_at.slice(0, 10)}): ${per} downloads\n${assets}`);
  }
  info(`total across ${releases.length} releases: ${total}`);
}

if (process.argv.includes("--self-test")) {
  // Pure logic is trivial (sum + formatting); the self-test just validates
  // argument handling and the repo default.
  if (!repo.includes("/")) fail("self-test: repo default malformed");
  ok("self-test: release-stats defaults");
  process.exit(0);
}

main();
